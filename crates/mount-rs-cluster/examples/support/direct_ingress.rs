//! Benchmark-only callers inside server processes, with optional public HTTP sockets.
use super::*;
use http_body_util::BodyExt;
use mount_rs_http::HttpRequestHandler;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub requests: Vec<Call>,
    pub samples: usize,
    #[serde(default)]
    pub skip_warmup: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub volume_index: Option<usize>,
    pub path: String,
    pub session: Option<String>,
    #[serde(default)]
    pub body: Vec<u8>,
    pub bearer: String,
    pub range: Option<String>,
    pub write_path: Option<String>,
    pub write_url: Option<String>,
    pub write_bearer: Option<String>,
    pub connection_warmup: Option<(String, String)>,
    pub sequential_blocks: Option<(usize, u64)>,
    pub expected_status: u16,
    pub expected_body: Vec<u8>,
}
async fn dispatch(
    ingress: &BenchmarkIngress,
    public: Option<&(String, reqwest::Client)>,
    request: http::Request<mount_rs_http::HttpBody>,
) -> std::result::Result<
    (
        http::StatusCode,
        bytes::Bytes,
        forward_samples::ClientTiming,
    ),
    &'static str,
> {
    let ingress_duration = |headers: &http::HeaderMap| {
        headers
            .get("x-mount-audit-ingress-ns")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
    };
    let started = Instant::now();
    let mut polls = super::request_detail::ClientPolls::default();
    let (status, ingress_ns, body, headers_elapsed, body_elapsed) =
        if let Some((url, client)) = public {
            let (parts, body) = request.into_parts();
            let body = body
                .collect()
                .await
                .map_err(|_| "request body failed")?
                .to_bytes();
            let traced = parts.headers.contains_key(forward_samples::HEADER);
            let send = client
                .request(parts.method, format!("{url}{}", parts.uri))
                .headers(parts.headers)
                .body(body)
                .send();
            let response = if traced {
                polls.observe(started, send).await
            } else {
                send.await
            }
            .map_err(|_| "public request failed")?;
            let headers_elapsed = started.elapsed();
            let status = response.status();
            let ingress_ns = ingress_duration(response.headers());
            let body_started = Instant::now();
            let body = response
                .bytes()
                .await
                .map_err(|_| "public response failed")?;
            (
                status,
                ingress_ns,
                body,
                headers_elapsed,
                body_started.elapsed(),
            )
        } else {
            let response = ingress.handle(request).await;
            let headers_elapsed = started.elapsed();
            let (parts, body) = response.into_parts();
            let body_started = Instant::now();
            let body = body.collect().await.map_err(|_| "body failed")?.to_bytes();
            (
                parts.status,
                ingress_duration(&parts.headers),
                body,
                headers_elapsed,
                body_started.elapsed(),
            )
        };
    let nanos = |duration: Duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
    Ok((
        status,
        body,
        forward_samples::ClientTiming {
            started: Some(started),
            polls,
            response_headers_ns: nanos(headers_elapsed),
            response_body_ns: nanos(body_elapsed),
            ingress_ns,
        },
    ))
}
pub async fn run(
    ingress: Arc<BenchmarkIngress>,
    batch: Batch,
    public_url: Option<String>,
) -> Result<Value> {
    if batch.requests.is_empty()
        || batch.requests.len() > 100
        || !(1..=10000).contains(&batch.samples)
    {
        return Err("direct ingress batch outside limits".into());
    }
    // Validate every caller before spawning: a failing caller must not leave
    // its peers waiting forever at the per-burst barrier.
    for call in &batch.requests {
        if call.expected_body.len() > 1024 * 1024 || !call.path.starts_with('/') {
            return Err("direct ingress request outside limits".into());
        }
        if let Some((client, _)) = call.sequential_blocks
            && (!batch.skip_warmup || batch.samples > 32 || client >= 100 || call.session.is_none())
        {
            return Err("sequential blocks require a warmed session and at most 32 blocks".into());
        }
        let _: http::Uri = call.path.parse()?;
        let _: http::HeaderValue = format!("Bearer {}", call.bearer).parse()?;
        if let Some(range) = &call.range {
            let _: http::HeaderValue = range.parse()?;
        }
        if call.body.len() > 8192 {
            return Err("direct request body too large".into());
        }
        if let Some(session) = &call.session {
            let _: http::HeaderValue = session.parse()?;
        }
        if let Some(path) = &call.write_path {
            let _: http::Uri = path.parse()?;
            let bearer = call
                .write_bearer
                .as_ref()
                .ok_or("missing write credential")?;
            let _: http::HeaderValue = format!("Bearer {bearer}").parse()?;
        }
        http::StatusCode::from_u16(call.expected_status)?;
    }
    let public_clients = (0..batch.requests.len())
        .map(|_| {
            public_url
                .as_ref()
                .map(|url| audit_http_client().map(|client| (url.clone(), client)))
                .transpose()
        })
        .collect::<Result<Vec<_>>>()?;
    let write_clients = batch
        .requests
        .iter()
        .map(|call| {
            call.write_url
                .as_ref()
                .map(|url| audit_http_client().map(|client| (url.clone(), client)))
                .transpose()
        })
        .collect::<Result<Vec<_>>>()?;
    // Finish all preflights before starting the measured callers and their barrier.
    let mut preflights = tokio::task::JoinSet::new();
    for (call, public) in batch.requests.iter().zip(&public_clients).chain(
        batch
            .requests
            .iter()
            .zip(&write_clients)
            .filter(|(_, writer)| writer.is_some()),
    ) {
        if let Some((path, bearer)) = &call.connection_warmup {
            let (url, client) = public
                .as_ref()
                .ok_or("connection warmup requires public HTTP")?;
            let request = client.get(format!("{url}{path}")).bearer_auth(bearer);
            preflights.spawn(async move {
                let response = request.send().await?.error_for_status()?;
                let body = response.bytes().await?;
                if serde_json::from_slice::<Value>(&body)?["size"]
                    .as_u64()
                    .is_none()
                {
                    return Err("invalid connection warmup stat".into());
                }
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }
    }
    while let Some(result) = preflights.join_next().await {
        result??;
    }
    let capture = if forward_samples::enabled()? {
        Some(Arc::new(forward_samples::Capture::new(
            batch.requests.len() * (batch.samples + 1),
        )?))
    } else {
        None
    };
    ingress.capture.store(capture.clone());

    let write_limit = std::env::var("AUDIT_DIRECT_WRITE_LIMIT")
        .ok()
        .map(|raw| raw.parse::<usize>())
        .transpose()?;
    if write_limit.is_some_and(|n| !(1..=100).contains(&n)) {
        return Err("AUDIT_DIRECT_WRITE_LIMIT must be 1..100".into());
    }
    let pairs_per_second = std::env::var("AUDIT_PAIRS_PER_SECOND_PER_WORKER")
        .ok()
        .map(|v| v.parse::<u32>())
        .transpose()?;
    if pairs_per_second.is_some_and(|rate| !(1..=10000).contains(&rate) || !batch.skip_warmup) {
        return Err("paced arrivals require rate 1..10000, connection warmup".into());
    }
    let write_stride = std::env::var("AUDIT_INDEPENDENT_WRITE_STRIDE")
        .ok()
        .map(|s| s.parse::<usize>())
        .transpose()?;
    if write_stride.is_some_and(|stride| {
        stride == 0
            || !batch.samples.is_multiple_of(stride)
            || pairs_per_second.is_none()
            || batch.requests.iter().any(|call| call.write_path.is_none())
    }) {
        return Err("independent writes require paced mixed reads and a positive stride dividing the sample count".into());
    }
    let participants = batch.requests.len() * if write_stride.is_some() { 2 } else { 1 };
    let barrier = Arc::new(tokio::sync::Barrier::new(participants));
    let client_count = batch.requests.len();
    let epoch = Arc::new(std::sync::OnceLock::new());
    let write_slots = write_limit.map(|n| Arc::new(tokio::sync::Semaphore::new(n)));
    let mut tasks = tokio::task::JoinSet::new();
    for (caller, ((call, public), writer)) in batch
        .requests
        .into_iter()
        .zip(public_clients)
        .zip(write_clients)
        .enumerate()
    {
        let epoch = epoch.clone();
        let traced = capture.is_some();
        let ingress = ingress.clone();
        let barrier = barrier.clone();
        let write_slots = write_slots.clone();
        tasks.spawn(async move {
            let mut launch_lags = Vec::new();
            let mut latencies = Vec::new();
            let mut errors = 0;
            let mut write_latencies = Vec::new();
            let mut write_errors = 0;
            let mut observed = Vec::new();
            let contents = call.sequential_blocks.map(|(client, _)| payload(client, batch.samples * 4096));
            let submit_write = async |iteration: usize| {
                    let Some(path) = &call.write_path else {
                        return None;
                    };
                    let request = http::Request::builder()
                        .method("PUT")
                        .uri(path)
                        .header(
                            "authorization",
                            format!("Bearer {}", call.write_bearer.as_ref().unwrap()),
                        )
                        .body(
                            http_body_util::Full::new(bytes::Bytes::from(vec![
                                (iteration % 251)
                                    as u8;
                                4096
                            ]))
                            .map_err(|never| -> mount_rs_http::BoxError { match never {} })
                            .boxed_unsync(),
                        )
                        .expect("validated writer request");
                    let started = Instant::now();
                    let result = timeout(Duration::from_secs(30), async {
                        let _permit = match &write_slots {
                            Some(slots) => Some(slots.acquire().await.expect("writer semaphore remains open")),
                            None => None,
                        };
                        matches!(dispatch(&ingress, writer.as_ref().or(public.as_ref()),request).await, Ok((status,_,_)) if status.is_success())
                    })
                    .await;
                    Some((
                        matches!(result, Ok(true)),
                        started.elapsed().as_secs_f64() * 1000.0,
                    ))
                };
            let independent_writes = async {
                let mut results = Vec::new();
                if let Some(stride) = write_stride {
                    barrier.wait().await;
                    let start = *epoch.get_or_init(tokio::time::Instant::now);
                    let rate = pairs_per_second.expect("validated independent pace");
                    for n in 0..(batch.samples / stride) {
                        let slot = (n * client_count + caller) * stride;
                        let due = start + Duration::from_nanos(slot as u64 * 1_000_000_000 / u64::from(rate));
                        tokio::time::sleep_until(due).await;
                        let lag = due.elapsed().as_secs_f64() * 1000.0;
                        results.push((lag, submit_write((n + 1) * stride).await.expect("validated write")));
                    }
                }
                results
            };
            let read_loop = async {
            for iteration in usize::from(batch.skip_warmup)..=batch.samples {
                let mut request = http::Request::builder()
                    .uri(&call.path)
                    .header("authorization", format!("Bearer {}", call.bearer));
                let sample_id = caller * (batch.samples + 1) + iteration;
                if traced { request = request.header(forward_samples::HEADER, sample_id); }
                if let Some(range) = &call.range {
                    request = request.header("range", range);
                }
                if let Some(session) = &call.session {
                    request = request.method("POST").header("x-mount-session", session);
                }
                let (body, expected) = if let (Some((_, handle)), Some(contents)) = (call.sequential_blocks, &contents) {
                    let offset = (iteration - 1) * 4096;
                    (json!({"operation":"read","handle":handle,"length":4096,"position":offset}).to_string().into_bytes(), &contents[offset..offset+4096])
                } else {
                    (call.body.clone(), call.expected_body.as_slice())
                };
                let request = request.body(
                    http_body_util::Full::new(bytes::Bytes::from(body))
                        .map_err(|never| -> mount_rs_http::BoxError { match never {} })
                        .boxed_unsync(),
                )?;
                if pairs_per_second.is_none() || iteration == 1 {
                    barrier.wait().await;
                }
                if let Some(rate) = pairs_per_second {
                    let start = *epoch.get_or_init(tokio::time::Instant::now);
                    let slot = (iteration - 1) * client_count + caller;
                    let due = start + Duration::from_nanos(slot as u64 * 1_000_000_000 / u64::from(rate));
                    tokio::time::sleep_until(due).await;
                    launch_lags.push(due.elapsed().as_secs_f64() * 1000.0);
                }
                let read = async {
                    let started = Instant::now();
                    let result = timeout(Duration::from_secs(30), async {
                        let (status, bytes, timing) = dispatch(&ingress, public.as_ref(),request).await?;
                        Ok::<_, &'static str>((
                            status.as_u16() == call.expected_status && bytes.as_ref() == expected, timing,
                        ))
                    })
                    .await;
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    (result, elapsed)
                };
                let write = async {
                    if write_stride.is_none() { submit_write(iteration).await } else { None }
                };
                let ((result, elapsed), written) = tokio::join!(read, write);
                if let Some((success, elapsed)) = written {
                    if success {
                        if iteration != 0 {
                            write_latencies.push(elapsed);
                        }
                    } else {
                        write_errors += 1;
                    }
                }

                if let Ok(Ok((true, timing))) = result {
                    if iteration != 0 {
                        latencies.push(elapsed);
                        if traced { observed.push((sample_id, elapsed, timing)); }
                    }
                } else {
                    errors += 1;
                }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            };
            let (completed, independent) = tokio::join!(read_loop, independent_writes);
            completed?;
            let mut write_lags = Vec::new();
            for (lag, (success, elapsed)) in independent {
                write_lags.push(lag);
                if success { write_latencies.push(elapsed); } else { write_errors += 1; }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((
                latencies,
                errors,
                write_latencies,
                write_errors,
                observed,
                launch_lags, write_lags, call.volume_index,
            ))
        });
    }
    let mut latencies = Vec::new();
    let mut errors = 0;
    let mut write_latencies = Vec::new();
    let mut write_errors = 0;
    let mut observed = Vec::new();
    let mut launch_lags = Vec::new();
    let mut write_lags = Vec::new();
    let mut sample_volumes = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (
            mut samples,
            failed,
            mut writes,
            failed_writes,
            mut request_samples,
            mut lags,
            mut write_lateness,
            volume_index,
        ) = result??;
        sample_volumes.extend(std::iter::repeat_n(volume_index, samples.len()));
        write_lags.append(&mut write_lateness);
        launch_lags.append(&mut lags);
        observed.append(&mut request_samples);
        write_latencies.append(&mut writes);
        write_errors += failed_writes;
        latencies.append(&mut samples);
        errors += failed;
    }
    ingress.capture.store(None);
    super::request_detail::H2Clock::stop();
    let forward_samples = capture.map(|capture| capture.report(observed));
    Ok(
        json!({"sample_volumes":sample_volumes,"forward_samples":forward_samples,"measurement":"in-process ingress through response body", "public_socket_included":public_url.is_some(),
        "independent_write_stride":write_stride,"write_launch_lag_ms":write_stride.map(|_|summary(write_lags)),"pairs_per_second_per_worker":pairs_per_second,"pair_launch_lag_ms":pairs_per_second.map(|_|summary(launch_lags)),"write_limit_per_worker":write_limit,"write_ms":summary(write_latencies),"write_errors":write_errors,"successful_reads":latencies.len(),"errors":errors,"read_ms":summary(latencies)}),
    )
}

async fn redis_counters(connection: &mut redis::aio::MultiplexedConnection) -> Result<Value> {
    let info: String = redis::cmd("INFO")
        .arg("all")
        .query_async(connection)
        .await?;
    let value = |key: &str| info.lines().find_map(|line| line.strip_prefix(key));
    let total: u64 = value("total_commands_processed:")
        .ok_or("Redis total missing")?
        .parse()?;
    let eval: u64 = value("cmdstat_eval:calls=")
        .and_then(|v| v.split(',').next())
        .unwrap_or("0")
        .parse()?;
    let cpu = |key: &str| -> Result<f64> { Ok(value(key).ok_or("Redis CPU missing")?.parse()?) };
    Ok(
        json!({"total":total,"eval":eval,"cpu_seconds":cpu("used_cpu_sys:")? + cpu("used_cpu_user:")?}),
    )
}

pub async fn controller(
    workers: &mut [Worker],
    run: &str,
    clients: usize,
    samples: usize,
) -> Result<Vec<Value>> {
    let owner_snapshots = match std::env::var("AUDIT_OWNER_SNAPSHOTS").as_deref() {
        Ok("0") => false,
        Ok("1") | Err(_) => true,
        _ => return Err("AUDIT_OWNER_SNAPSHOTS must be 0 or 1".into()),
    };
    let mut rows = Vec::new();
    let mut redis_monitor = if std::env::var("AUDIT_REDIS_COUNTERS").as_deref() == Ok("1") {
        Some(
            redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL")?)?
                .get_multiplexed_async_connection()
                .await?,
        )
    } else {
        None
    };
    let cache_mode = std::env::var("AUDIT_DIRECT_CACHE").unwrap_or_else(|_| "warm".into());
    let clear_command = match cache_mode.as_str() {
        "warm" => None,
        "cleared-ram" => Some("clear"),
        "cleared-ram-and-disk" => Some("clear-all"),
        _ => return Err("invalid AUDIT_DIRECT_CACHE".into()),
    };
    let sequential = samples > 1
        && std::env::var_os("AUDIT_PAIRS_PER_SECOND_PER_WORKER").is_some()
        && session_rpc_enabled()?
        && std::env::var("AUDIT_DIRECT_STAT_WARMUP").as_deref() == Ok("1")
        && samples <= 32;
    if clear_command.is_some() && samples != 1 && !sequential {
        return Err("evicted direct batches require AUDIT_SAMPLES=1 so every measured read follows eviction".into());
    }
    let stat_warmup = match std::env::var("AUDIT_DIRECT_STAT_WARMUP").as_deref() {
        Ok("1") => true,
        Ok("0") | Err(_) => false,
        _ => return Err("AUDIT_DIRECT_STAT_WARMUP must be 0 or 1".into()),
    };
    let socket_snapshot = || -> Result<Option<String>> {
        if std::env::var("AUDIT_UDP_DROPS").as_deref() == Ok("1") {
            Ok(Some(std::fs::read_to_string("/proc/net/udp")?))
        } else {
            Ok(None)
        }
    };
    let write_route = std::env::var("AUDIT_WRITE_ROUTE").unwrap_or_else(|_| "shared".into());
    if !matches!(write_route.as_str(), "shared" | "owner" | "ingress") {
        return Err("AUDIT_WRITE_ROUTE must be shared, owner or ingress".into());
    }
    if write_route != "shared"
        && (!stat_warmup || std::env::var("AUDIT_DIRECT_PUBLIC").as_deref() != Ok("1"))
    {
        return Err("dedicated writer requires public HTTP and stat warmup".into());
    }
    for metadata in [true, false] {
        for forwarded in [false, true] {
            eprintln!(
                "direct phase setup clients={clients} metadata={metadata} forwarded={forwarded}"
            );
            if !forwarded && std::env::var("AUDIT_FORWARDED_ONLY").as_deref() == Ok("1") {
                continue;
            }
            if forwarded && workers.len() == 1 {
                continue;
            }
            let mut groups: Vec<Vec<Value>> = (0..workers.len()).map(|_| Vec::new()).collect();
            let client = audit_http_client()?;
            let mut sessions = Vec::new();
            for i in 0..clients {
                let owner = i % workers.len();
                let ingress = (owner + usize::from(forwarded)) % workers.len();
                let path = format!(
                    "/v1/drives/{}/{}/data",
                    volume(i),
                    if metadata { "stat" } else { "fs" }
                );
                let mut request = client
                    .get(format!("{}{}", workers[owner].url, path))
                    .bearer_auth(token(run, i));
                if !metadata {
                    request = request.header("range", "bytes=0-4095");
                }
                let response = request.send().await?;
                let status = response.status();
                if !status.is_success() {
                    return Err("direct fixture read failed".into());
                }
                let expected = response.bytes().await?;
                if !metadata && expected.as_ref() != payload(i, 4096) {
                    return Err("direct fixture content mismatch".into());
                }
                let expected_size = if metadata {
                    serde_json::from_slice::<Value>(&expected)?["size"].as_u64()
                } else {
                    None
                };
                if session_rpc_enabled()? {
                    let lifecycle =
                        format!("{}/v1/drives/{}/sessions", workers[ingress].url, volume(i));
                    let credential = session_token(run, i);
                    let response = client
                        .post(&lifecycle)
                        .bearer_auth(&credential)
                        .send()
                        .await?;
                    if response.status() != 201 {
                        return Err("direct session creation failed".into());
                    }
                    let created: Value = serde_json::from_slice(&response.bytes().await?)?;
                    let session = created["session"]
                        .as_str()
                        .ok_or("missing session token")?
                        .to_owned();
                    let path = format!("/v1/drives/{}/sessions/handles", volume(i));
                    let url = format!("{}{}", workers[ingress].url, path);
                    let response = client.post(&url).bearer_auth(&credential).header("x-mount-session", &session)
                        .body(json!({"operation":"open","path":"/data","mode":0,"flags":{"read":true,"write":false,"create":false,"truncate":false,"append":false,"exclusive":false}}).to_string()).send().await?;
                    if response.status() != 200 {
                        return Err("direct session open failed".into());
                    }
                    let opened: Value = serde_json::from_slice(&response.bytes().await?)?;
                    let handle = opened["handle"].as_u64().ok_or("missing handle")?;
                    let body = if metadata {
                        json!({"operation":"path","request":{"action":"stat","path":"/data"}})
                    } else {
                        json!({"operation":"read","handle":handle,"length":4096,"position":0})
                    }
                    .to_string();
                    let response = client
                        .post(&url)
                        .bearer_auth(&credential)
                        .header("x-mount-session", &session)
                        .body(body.clone())
                        .send()
                        .await?;
                    if response.status() != 200 {
                        return Err("direct session fixture read failed".into());
                    }
                    let expected = response.bytes().await?;
                    if !metadata && expected.as_ref() != payload(i, 4096) {
                        return Err("direct session content mismatch".into());
                    }
                    if metadata {
                        let value: Value = serde_json::from_slice(&expected)?;
                        if value["size"].as_u64() != expected_size || expected_size.is_none() {
                            return Err("direct session metadata size mismatch".into());
                        }
                    }
                    groups[ingress].push(json!({"volume_index":i,"path":path,"session":session,"body":body.into_bytes(),"bearer":credential,"range":null,"expected_status":200,"expected_body":expected.to_vec()}));
                    if sequential && !metadata {
                        groups[ingress].last_mut().unwrap()["sequential_blocks"] =
                            json!([i, handle]);
                    }
                    sessions.push((lifecycle, credential, session));
                } else {
                    groups[ingress].push(json!({"volume_index":i,"path":path,"bearer":token(run,i),"range":if metadata {None} else {Some("bytes=0-4095")},"expected_status":status.as_u16(),"expected_body":expected.to_vec()}));
                }
                if stat_warmup {
                    let call = groups[ingress].last_mut().ok_or("missing warmup caller")?;
                    call["connection_warmup"] =
                        json!([format!("/v1/drives/{}/stat/data", volume(i)), token(run, i)]);
                }
            }
            let mixed = std::env::var("AUDIT_DIRECT_MIXED").as_deref() == Ok("1");
            if mixed {
                for requests in &mut groups {
                    for call in requests {
                        let path = call["path"].as_str().unwrap();
                        let prefix = path.split('/').take(4).collect::<Vec<_>>().join("/");
                        let index = (0..clients)
                            .find(|i| prefix == format!("/v1/drives/{}", volume(*i)))
                            .ok_or("unknown write volume")?;
                        call["write_path"] = json!(format!("{prefix}/fs/direct-scratch"));
                        call["write_bearer"] = json!(token(run, index));
                        if write_route != "shared" {
                            let offset = usize::from(forwarded && write_route == "ingress");
                            call["write_url"] =
                                json!(workers[(index + offset) % workers.len()].url);
                        }
                    }
                }
            }
            let probe_enabled = match std::env::var("AUDIT_SCHEDULER_PROBE").as_deref() {
                Ok("1") => true,
                Ok("0") | Err(_) => false,
                _ => return Err("AUDIT_SCHEDULER_PROBE must be 0 or 1".into()),
            };
            let disk_enabled = audit_disk_cache()?;
            if let Some(command) = clear_command {
                for reply in probe_command(workers, command).await? {
                    if reply["cleared"] != true {
                        return Err("worker did not confirm direct cache eviction".into());
                    }
                }
            }
            let per_volume = std::env::var("AUDIT_VOLUME_READ_STATS").as_deref() == Ok("1");
            let volume_before = if per_volume {
                Some(probe_command(workers, "volume-read-stats").await?)
            } else {
                None
            };
            let cache_before = cache_totals(workers).await?;
            let has_gate = ingress_admission_config()?.is_some();
            if has_gate {
                probe_command(workers, "admission-stats").await?;
            }
            let before = probe_command(workers, "forward-stats").await?;
            let owner_before = if owner_snapshots {
                Some(probe_command(workers, "owner-stats").await?)
            } else {
                None
            };
            if probe_enabled {
                probe_command(workers, "probe-start").await?;
            }
            eprintln!(
                "direct phase measurement clients={clients} metadata={metadata} forwarded={forwarded} samples={samples}"
            );
            let redis_before = if let Some(conn) = &mut redis_monitor {
                Some(redis_counters(conn).await?)
            } else {
                None
            };
            let udp_before = socket_snapshot()?;
            let phase_start = Instant::now();
            for (worker, requests) in workers.iter_mut().zip(&groups) {
                if !requests.is_empty() {
                    worker
                        .send(&format!(
                            "direct-ingress {}",
                            json!({"requests":requests,"samples":samples,"skip_warmup":clear_command.is_some() || stat_warmup})
                        ))
                        .await?;
                }
            }
            let mut measurements = Vec::new();
            for (worker, requests) in workers.iter_mut().zip(&groups) {
                if !requests.is_empty() {
                    // A batch contains sequential read/write bursts. Individual RPC deadlines stay fixed.
                    measurements.push(
                        worker
                            .read_with_timeout((samples as u64 * 31 + 60).max(120))
                            .await?,
                    );
                }
            }
            if measurements
                .iter()
                .any(|m| m["direct_batch_failed"] == true)
            {
                let health = probe_command(workers, "health").await?;
                eprintln!(
                    "direct workload failure health: {}",
                    serde_json::to_string(&health)?
                );
                return Err("direct workload batch failed".into());
            }
            let phase_seconds = phase_start.elapsed().as_secs_f64();
            let udp_after = socket_snapshot()?;
            let redis_after = if let Some(conn) = &mut redis_monitor {
                Some(redis_counters(conn).await?)
            } else {
                None
            };
            let scheduler = if probe_enabled {
                probe_command(workers, "probe-stop").await?
            } else {
                Vec::new()
            };
            let owner_after = if owner_snapshots {
                Some(probe_command(workers, "owner-stats").await?)
            } else {
                None
            };
            let gate_stats = if has_gate {
                probe_command(workers, "admission-stats").await?
            } else {
                Vec::new()
            };
            let after = probe_command(workers, "forward-stats").await?;
            let volume_after = if per_volume {
                Some(probe_command(workers, "volume-read-stats").await?)
            } else {
                None
            };
            let cache_after = cache_totals(workers).await?;
            eprintln!(
                "direct phase validation clients={clients} metadata={metadata} forwarded={forwarded}"
            );
            if mixed {
                for i in 0..clients {
                    let response = client
                        .get(format!(
                            "{}/v1/drives/{}/fs/direct-scratch",
                            workers[i % workers.len()].url,
                            volume(i)
                        ))
                        .bearer_auth(token(run, i))
                        .send()
                        .await?;
                    if !response.status().is_success() {
                        return Err(format!(
                            "direct write validation read returned {} for volume {i}",
                            response.status()
                        )
                        .into());
                    }
                    if response.bytes().await?.as_ref() != vec![(samples % 251) as u8; 4096] {
                        return Err("direct write final contents mismatch".into());
                    }
                }
            }
            for (url, credential, session) in sessions {
                let response = client
                    .delete(url)
                    .bearer_auth(credential)
                    .header("x-mount-session", session)
                    .send()
                    .await?;
                if response.status() != 204 {
                    return Err("direct session cleanup failed".into());
                }
                response.bytes().await?;
            }

            let errors: u64 = measurements
                .iter()
                .map(|m| m["errors"].as_u64().unwrap() + m["write_errors"].as_u64().unwrap())
                .sum();
            let values = measurements
                .iter()
                .flat_map(|m| {
                    m["read_ms"]["samples_ms"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_f64().unwrap())
                })
                .collect();
            rows.push(json!({"block_cache":cache_after.change_since(&cache_before),
                "disk_cache":disk_enabled.then(|| cache_after.disk_change_since(&cache_before)),
                "udp_drop_snapshots":udp_before.map(|before|json!({"before":before,"after":udp_after,"sockets":workers.iter().map(|w| (&w.pid,&w.udp_sockets)).collect::<Vec<_>>()})),
                "volume_reads_before":volume_before,"volume_reads_after":volume_after,"write_route":write_route,"connection_stat_warmup":stat_warmup,
                "cache_counter_scope":if stat_warmup {"connection stat preflights, measured reads and any paired writes"} else if clear_command.is_some() {"measured reads and any paired writes"} else {"worker batches including warmup reads and any paired writes"},
                "block_cache_backing":if disk_enabled {"disk-cache"} else {"authoritative-provider"},
                "redis_before":redis_before,"redis_after":redis_after,"phase_seconds":phase_seconds,"session_rpc":session_rpc_enabled()?,"clients":clients,"servers":workers.len(),"path":if forwarded {"forwarded"}else{"local-owner"},"operation":if metadata {"metadata-stat"}else{"read-4k"},"errors":errors,"read_ms":summary(values),"admission":gate_stats,"forward_before":before,"forward_after":after,"owner_before":owner_before,"owner_after":owner_after,"scheduler_probe":scheduler,"workers":measurements,"public_socket_included":std::env::var("AUDIT_DIRECT_PUBLIC").as_deref()==Ok("1"),"cache":cache_mode,"workload":if mixed {"mixed"}else{"read-only"}}));
            if errors != 0 {
                return Err("direct ingress measured errors".into());
            }
        }
    }
    Ok(rows)
}
