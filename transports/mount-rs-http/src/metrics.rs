//! Counters for an [`HttpServer`](crate::HttpServer), and a listener that
//! serves them in the Prometheus text format.
//!
//! [`HttpMetrics`] is an [`AccessLog`]: it is told about each request as
//! the access log is, and so counts what the server measured, labelled by
//! what the server chose. A label is a route template, a method from a
//! fixed list, a status class or a configured drive id. No path, token or
//! header text reaches a label, so the number of series is bounded however
//! clients behave.
//!
//! The metrics are served by their own listener, [`MetricsServer`], never
//! by the API's: the API listener is the one a reverse proxy publishes, and
//! the metrics need no token.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, header};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::server::{AccessLog, AccessRecord, DriveConfig, HttpServerError};

/// The `Content-Type` of the Prometheus text exposition format.
pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Upper bounds of the request duration buckets, in seconds, as they are
/// written in `le` labels, and in microseconds.
const BUCKETS: [(&str, u64); 14] = [
    ("0.005", 5_000),
    ("0.01", 10_000),
    ("0.025", 25_000),
    ("0.05", 50_000),
    ("0.1", 100_000),
    ("0.25", 250_000),
    ("0.5", 500_000),
    ("1", 1_000_000),
    ("2.5", 2_500_000),
    ("5", 5_000_000),
    ("10", 10_000_000),
    ("30", 30_000_000),
    ("60", 60_000_000),
    ("300", 300_000_000),
];

/// Connections the metrics listener serves at once. A scraper needs one.
const MAX_METRICS_CONNECTIONS: usize = 8;
/// How long a scraper may take to send its request.
const METRICS_HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// The methods a request is counted under, in the order they are written.
/// A fixed list, so that a client cannot create a series by inventing a
/// method.
const METHODS: [&str; 8] = [
    "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "other",
];

/// The status classes a request is counted under, in the order they are
/// written. `none` is a request that ended before the server produced a
/// response.
const STATUSES: [&str; 7] = ["1xx", "2xx", "3xx", "4xx", "5xx", "none", "other"];

/// Everything counted for one route template. Every series is a counter of
/// its own, so a request adds to them without waiting for another request
/// or for a scrape.
struct RouteCounts {
    route: &'static str,
    /// By method, then status class, as indexed in [`METHODS`] and
    /// [`STATUSES`].
    requests: [[AtomicU64; STATUSES.len()]; METHODS.len()],
    /// Requests whose duration was at most each bucket's bound and over the
    /// one before, not cumulative: summed when rendered. The last counts
    /// the requests over every bound. The histogram's count is the sum of
    /// these and is not kept separately, so a scrape that runs beside a
    /// request can never show a bucket above the count.
    buckets: [AtomicU64; BUCKETS.len() + 1],
    sum_micros: AtomicU64,
}

impl RouteCounts {
    fn new(route: &'static str) -> Self {
        Self {
            route,
            requests: Default::default(),
            buckets: Default::default(),
            sum_micros: AtomicU64::new(0),
        }
    }

    fn count(&self, method: usize, status: usize, micros: u64) {
        self.requests[method][status].fetch_add(1, Ordering::Relaxed);
        let bucket = BUCKETS
            .iter()
            .position(|(_, bound)| micros <= *bound)
            .unwrap_or(BUCKETS.len());
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        // Saturating, as the sum always was. The closure never refuses.
        let _ = self
            .sum_micros
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |sum| {
                Some(sum.saturating_add(micros))
            });
    }
}

/// Where `method` is in [`METHODS`].
fn method_index(method: &str) -> usize {
    METHODS
        .iter()
        .position(|known| *known == method)
        .unwrap_or(METHODS.len() - 1)
}

/// Where `status` is in [`STATUSES`].
fn status_index(status: Option<u16>) -> usize {
    match status.map(|status| status / 100) {
        Some(class @ 1..=5) => usize::from(class) - 1,
        None => 5,
        Some(_) => 6,
    }
}

/// A label value, escaped as the text format requires.
fn escaped(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

type Reading = Arc<dyn Fn() -> u64 + Send + Sync>;

/// What an HTTP server has done, counted for a metrics scrape.
///
/// Give it to the server as its access log, or as one of them, and to a
/// [`MetricsServer`] to be scraped.
pub struct HttpMetrics {
    version: String,
    started: SystemTime,
    /// One entry per route template that has been counted, ordered by
    /// template. Route templates are the server's own constants, so the
    /// list stops growing once each has been seen; until then a new one is
    /// added by copying the list and swapping it in. A request for a route
    /// already listed reads the list and writes only that route's counters.
    routes: ArcSwap<Vec<Arc<RouteCounts>>>,
    in_flight: AtomicI64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    /// Added to while the server starts, read at each scrape.
    drives: ArcSwap<Vec<DriveConfig>>,
    access_log_dropped: OnceLock<Reading>,
}

impl std::fmt::Debug for HttpMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpMetrics")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl HttpMetrics {
    /// Start counting now. `version` is reported as the build's version.
    pub fn new(version: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            version: version.into(),
            started: SystemTime::now(),
            routes: ArcSwap::from_pointee(Vec::new()),
            in_flight: AtomicI64::new(0),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            drives: ArcSwap::from_pointee(Vec::new()),
            access_log_dropped: OnceLock::new(),
        })
    }

    /// Report whether `drive` is ready, as `/readyz` counts it, under its
    /// drive id.
    pub fn observe_drive(&self, drive: &DriveConfig) {
        self.drives.rcu(|drives| {
            let mut next = Vec::clone(drives);
            next.push(drive.clone());
            next
        });
    }

    /// The counters for `route`, adding them if this is the first request
    /// counted for it. Two first requests for one route end with the same
    /// counters: the one whose list was not swapped in finds the other's.
    fn add_route(&self, route: &'static str) -> Arc<RouteCounts> {
        let fresh = Arc::new(RouteCounts::new(route));
        let mut chosen = Arc::clone(&fresh);
        self.routes.rcu(
            |routes| match routes.binary_search_by(|listed| listed.route.cmp(route)) {
                Ok(found) => {
                    chosen = Arc::clone(&routes[found]);
                    Arc::clone(routes)
                }
                Err(place) => {
                    chosen = Arc::clone(&fresh);
                    let mut next = Vec::clone(routes);
                    next.insert(place, Arc::clone(&fresh));
                    Arc::new(next)
                }
            },
        );
        chosen
    }

    /// Report the lines an access log has dropped, read from `dropped` at
    /// each scrape. Only the first call counts.
    pub fn observe_access_log_dropped(&self, dropped: impl Fn() -> u64 + Send + Sync + 'static) {
        let _ = self.access_log_dropped.set(Arc::new(dropped));
    }

    /// Everything counted so far, in the Prometheus text format.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(8 * 1024);
        // Writing to a String cannot fail.
        let _ = self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) -> std::fmt::Result {
        // Each counter is read once. A request finishing during the scrape
        // may be in one family and not yet in another, as it may between
        // any two scrapes; within a histogram the count is the sum of the
        // buckets read, so the buckets never pass it.
        let routes = self.routes.load_full();
        out.push_str(
            "# HELP mount_rs_http_requests_total Requests finished, by route template, method and status class.\n\
             # TYPE mount_rs_http_requests_total counter\n",
        );
        for counts in routes.iter() {
            let route = counts.route;
            for (method, by_status) in METHODS.iter().zip(&counts.requests) {
                for (status, count) in STATUSES.iter().zip(by_status) {
                    let count = count.load(Ordering::Relaxed);
                    if count == 0 {
                        continue;
                    }
                    writeln!(
                        out,
                        "mount_rs_http_requests_total{{route=\"{route}\",method=\"{method}\",status=\"{status}\"}} {count}"
                    )?;
                }
            }
        }
        out.push_str(
            "# HELP mount_rs_http_request_duration_seconds Time from a request's arrival until its response body was sent or abandoned, by route template.\n\
             # TYPE mount_rs_http_request_duration_seconds histogram\n",
        );
        for counts in routes.iter() {
            let route = counts.route;
            let mut cumulative: u64 = 0;
            for ((bound, _), count) in BUCKETS.iter().zip(&counts.buckets) {
                cumulative += count.load(Ordering::Relaxed);
                writeln!(
                    out,
                    "mount_rs_http_request_duration_seconds_bucket{{route=\"{route}\",le=\"{bound}\"}} {cumulative}"
                )?;
            }
            let total = cumulative + counts.buckets[BUCKETS.len()].load(Ordering::Relaxed);
            let sum_micros = counts.sum_micros.load(Ordering::Relaxed);
            writeln!(
                out,
                "mount_rs_http_request_duration_seconds_bucket{{route=\"{route}\",le=\"+Inf\"}} {total}"
            )?;
            writeln!(
                out,
                "mount_rs_http_request_duration_seconds_sum{{route=\"{route}\"}} {}.{:06}",
                sum_micros / 1_000_000,
                sum_micros % 1_000_000
            )?;
            writeln!(
                out,
                "mount_rs_http_request_duration_seconds_count{{route=\"{route}\"}} {total}"
            )?;
        }
        writeln!(
            out,
            "# HELP mount_rs_http_requests_in_flight Requests that have arrived and are not finished.\n\
             # TYPE mount_rs_http_requests_in_flight gauge\n\
             mount_rs_http_requests_in_flight {}",
            self.in_flight.load(Ordering::Relaxed).max(0)
        )?;
        writeln!(
            out,
            "# HELP mount_rs_http_request_body_bytes_total Request body bytes read.\n\
             # TYPE mount_rs_http_request_body_bytes_total counter\n\
             mount_rs_http_request_body_bytes_total {}",
            self.bytes_in.load(Ordering::Relaxed)
        )?;
        writeln!(
            out,
            "# HELP mount_rs_http_response_body_bytes_total Response body bytes handed to connections.\n\
             # TYPE mount_rs_http_response_body_bytes_total counter\n\
             mount_rs_http_response_body_bytes_total {}",
            self.bytes_out.load(Ordering::Relaxed)
        )?;
        out.push_str(
            "# HELP mount_rs_http_drive_ready Whether the drive is ready as /readyz counts it: 1 ready, 0 not.\n\
             # TYPE mount_rs_http_drive_ready gauge\n",
        );
        let drives = self.drives.load_full();
        for drive in drives.iter() {
            writeln!(
                out,
                "mount_rs_http_drive_ready{{drive=\"{}\"}} {}",
                escaped(drive.id()),
                u8::from(drive.health().is_ok())
            )?;
        }
        if let Some(dropped) = self.access_log_dropped.get() {
            writeln!(
                out,
                "# HELP mount_rs_http_access_log_dropped_lines_total Access log lines dropped because the log's output was not being read.\n\
                 # TYPE mount_rs_http_access_log_dropped_lines_total counter\n\
                 mount_rs_http_access_log_dropped_lines_total {}",
                dropped()
            )?;
        }
        let started = self
            .started
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        writeln!(
            out,
            "# HELP process_start_time_seconds Start time of the process since the Unix epoch, in seconds.\n\
             # TYPE process_start_time_seconds gauge\n\
             process_start_time_seconds {}.{:03}",
            started.as_secs(),
            started.subsec_millis()
        )?;
        writeln!(
            out,
            "# HELP mount_rs_build_info The version of this build; the value is always 1.\n\
             # TYPE mount_rs_build_info gauge\n\
             mount_rs_build_info{{version=\"{}\"}} 1",
            escaped(&self.version)
        )
    }
}

impl AccessLog for HttpMetrics {
    fn begin(&self) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    fn record(&self, record: &AccessRecord<'_>) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.bytes_in.fetch_add(record.bytes_in, Ordering::Relaxed);
        self.bytes_out
            .fetch_add(record.bytes_out, Ordering::Relaxed);
        let micros = u64::try_from(record.duration.as_micros()).unwrap_or(u64::MAX);
        let (method, status) = (method_index(record.method), status_index(record.status));
        let routes = self.routes.load();
        match routes.binary_search_by(|listed| listed.route.cmp(record.route)) {
            Ok(found) => routes[found].count(method, status, micros),
            Err(_) => {
                drop(routes);
                self.add_route(record.route).count(method, status, micros);
            }
        }
    }
}

/// Several access logs as one: each is told about every request.
#[derive(Debug)]
pub struct AccessLogs(pub Vec<Arc<dyn AccessLog>>);

impl AccessLog for AccessLogs {
    fn begin(&self) {
        for log in &self.0 {
            log.begin();
        }
    }

    fn record(&self, record: &AccessRecord<'_>) {
        for log in &self.0 {
            log.record(record);
        }
    }
}

/// The listener that serves [`HttpMetrics`] at `GET /metrics`. Dropping it
/// stops it.
///
/// It asks for no token, so it refuses an address that is not loopback
/// unless told that the network to it is private.
#[derive(Debug)]
pub struct MetricsServer {
    address: SocketAddr,
    accept: tokio::task::JoinHandle<()>,
}

impl MetricsServer {
    /// Listen on `listen` and serve `metrics`.
    pub async fn start(
        listen: SocketAddr,
        allow_non_loopback: bool,
        metrics: Arc<HttpMetrics>,
    ) -> Result<Self, HttpServerError> {
        if !allow_non_loopback && !listen.ip().is_loopback() {
            return Err(HttpServerError::InsecureBind {
                host: listen.ip().to_string(),
            });
        }
        let listener = TcpListener::bind(listen)
            .await
            .map_err(HttpServerError::Bind)?;
        let address = listener.local_addr().map_err(HttpServerError::Io)?;
        let connections = Arc::new(Semaphore::new(MAX_METRICS_CONNECTIONS));
        let accept = tokio::spawn(async move {
            // Ends with the accept task, and with it every connection.
            let mut serving = tokio::task::JoinSet::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    // Out of descriptors, most likely: the API listener
                    // reports that. Try again shortly.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                while serving.try_join_next().is_some() {}
                // Over the limit the connection is closed unanswered.
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                    continue;
                };
                let metrics = Arc::clone(&metrics);
                serving.spawn(async move {
                    let _permit = permit;
                    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                        let response = respond(&request, &metrics);
                        async move { Ok::<_, std::convert::Infallible>(response) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .keep_alive(true)
                        .header_read_timeout(Some(METRICS_HEADER_TIMEOUT))
                        .timer(TokioTimer::new())
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Ok(Self { address, accept })
    }

    /// The address the listener is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for MetricsServer {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

fn respond<B>(request: &Request<B>, metrics: &HttpMetrics) -> Response<Full<Bytes>> {
    let text = |status: StatusCode, body: &'static str| {
        let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        response
    };
    if request.uri().path() != "/metrics" {
        return text(StatusCode::NOT_FOUND, "not found\n");
    }
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        let mut response = text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    let body = if head {
        Bytes::new()
    } else {
        Bytes::from(metrics.render())
    };
    let mut response = Response::new(Full::new(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(METRICS_CONTENT_TYPE),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        metrics: &HttpMetrics,
        method: &str,
        route: &'static str,
        status: Option<u16>,
        duration: Duration,
        bytes: (u64, u64),
    ) {
        metrics.begin();
        metrics.record(&AccessRecord {
            request_id: "id",
            method,
            route,
            drive: None,
            status,
            duration,
            bytes_in: bytes.0,
            bytes_out: bytes.1,
        });
    }

    fn value(text: &str, series: &str) -> Option<String> {
        text.lines()
            .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
            .map(str::to_owned)
    }

    #[test]
    fn requests_are_counted_by_route_method_and_status_class() {
        let metrics = HttpMetrics::new("1.2.3");
        let fs = "/v1/drives/{id}/fs/{path}";
        record(
            &metrics,
            "GET",
            fs,
            Some(200),
            Duration::from_millis(3),
            (0, 100),
        );
        record(
            &metrics,
            "GET",
            fs,
            Some(206),
            Duration::from_millis(30),
            (0, 50),
        );
        record(
            &metrics,
            "PUT",
            fs,
            Some(201),
            Duration::from_secs(2),
            (4096, 0),
        );
        record(
            &metrics,
            "GET",
            fs,
            Some(404),
            Duration::from_micros(5_000),
            (0, 40),
        );
        record(
            &metrics,
            "GET",
            "/readyz",
            Some(503),
            Duration::ZERO,
            (0, 10),
        );
        record(&metrics, "GET", fs, None, Duration::from_secs(1000), (0, 7));
        // A method a client made up does not get a series of its own.
        record(
            &metrics,
            "BREW",
            "unmatched",
            Some(404),
            Duration::ZERO,
            (0, 0),
        );
        record(
            &metrics,
            "STIR",
            "unmatched",
            Some(404),
            Duration::ZERO,
            (0, 0),
        );
        let text = metrics.render();
        let requests =
            |labels: &str| value(&text, &format!("mount_rs_http_requests_total{{{labels}}}"));
        assert_eq!(
            requests(&format!("route=\"{fs}\",method=\"GET\",status=\"2xx\"")).as_deref(),
            Some("2")
        );
        assert_eq!(
            requests(&format!("route=\"{fs}\",method=\"PUT\",status=\"2xx\"")).as_deref(),
            Some("1")
        );
        assert_eq!(
            requests(&format!("route=\"{fs}\",method=\"GET\",status=\"4xx\"")).as_deref(),
            Some("1")
        );
        assert_eq!(
            requests(&format!("route=\"{fs}\",method=\"GET\",status=\"none\"")).as_deref(),
            Some("1")
        );
        assert_eq!(
            requests("route=\"/readyz\",method=\"GET\",status=\"5xx\"").as_deref(),
            Some("1")
        );
        assert_eq!(
            requests("route=\"unmatched\",method=\"other\",status=\"4xx\"").as_deref(),
            Some("2")
        );
        assert!(!text.contains("BREW") && !text.contains("STIR"), "{text}");

        // The histogram is cumulative, and a bound includes its own value.
        let bucket = |le: &str| {
            value(
                &text,
                &format!(
                    "mount_rs_http_request_duration_seconds_bucket{{route=\"{fs}\",le=\"{le}\"}}"
                ),
            )
        };
        assert_eq!(bucket("0.005").as_deref(), Some("2"));
        assert_eq!(bucket("0.025").as_deref(), Some("2"));
        assert_eq!(bucket("0.05").as_deref(), Some("3"));
        assert_eq!(bucket("2.5").as_deref(), Some("4"));
        assert_eq!(bucket("300").as_deref(), Some("4"));
        assert_eq!(bucket("+Inf").as_deref(), Some("5"));
        assert_eq!(
            value(
                &text,
                &format!("mount_rs_http_request_duration_seconds_count{{route=\"{fs}\"}}")
            )
            .as_deref(),
            Some("5")
        );
        assert_eq!(
            value(
                &text,
                &format!("mount_rs_http_request_duration_seconds_sum{{route=\"{fs}\"}}")
            )
            .as_deref(),
            Some("1002.038000")
        );
        assert_eq!(
            value(&text, "mount_rs_http_request_body_bytes_total").as_deref(),
            Some("4096")
        );
        assert_eq!(
            value(&text, "mount_rs_http_response_body_bytes_total").as_deref(),
            Some("207")
        );
        assert_eq!(
            value(&text, "mount_rs_http_requests_in_flight").as_deref(),
            Some("0")
        );
        assert_eq!(
            value(&text, "mount_rs_build_info{version=\"1.2.3\"}").as_deref(),
            Some("1")
        );
        let started: f64 = value(&text, "process_start_time_seconds")
            .expect("start time")
            .parse()
            .expect("a number");
        assert!(started > 1_600_000_000.0, "{started}");
        // Off until an access log is observed.
        assert!(!text.contains("access_log_dropped"), "{text}");
    }

    #[test]
    fn a_request_is_in_flight_from_its_arrival_until_it_is_recorded() {
        let metrics = HttpMetrics::new("v");
        metrics.begin();
        metrics.begin();
        assert_eq!(
            value(&metrics.render(), "mount_rs_http_requests_in_flight").as_deref(),
            Some("2")
        );
        record(
            &metrics,
            "GET",
            "/healthz",
            Some(200),
            Duration::ZERO,
            (0, 0),
        );
        // That one began and ended; the two before it are still out.
        assert_eq!(
            value(&metrics.render(), "mount_rs_http_requests_in_flight").as_deref(),
            Some("2")
        );
    }

    #[test]
    fn the_text_is_well_formed_exposition() {
        let metrics = HttpMetrics::new("a\"b\\c\nd");
        metrics.observe_access_log_dropped(|| 9);
        record(
            &metrics,
            "GET",
            "/healthz",
            Some(200),
            Duration::from_millis(1),
            (0, 2),
        );
        let text = metrics.render();
        assert!(text.ends_with('\n'));
        assert!(
            text.contains("mount_rs_build_info{version=\"a\\\"b\\\\c\\nd\"} 1\n"),
            "{text}"
        );
        assert_eq!(
            value(&text, "mount_rs_http_access_log_dropped_lines_total").as_deref(),
            Some("9")
        );
        let mut typed = std::collections::BTreeSet::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let (name, kind) = rest.split_once(' ').expect("a type");
                assert!(matches!(kind, "counter" | "gauge" | "histogram"), "{line}");
                assert!(typed.insert(name.to_owned()), "typed twice: {line}");
                continue;
            }
            if line.starts_with("# HELP ") {
                continue;
            }
            // A sample: a name, optional labels, one space and a number.
            let (series, number) = line.rsplit_once(' ').expect("a sample");
            number.parse::<f64>().unwrap_or_else(|_| panic!("{line}"));
            let name = series.split('{').next().expect("a name");
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "{line}"
            );
            let family = ["_bucket", "_sum", "_count"]
                .iter()
                .find_map(|suffix| name.strip_suffix(suffix))
                .filter(|family| typed.contains(*family))
                .unwrap_or(name);
            assert!(typed.contains(family), "a sample before its TYPE: {line}");
            if let Some(labels) = series
                .strip_prefix(name)
                .filter(|labels| !labels.is_empty())
            {
                assert!(labels.starts_with('{') && labels.ends_with("\"}"), "{line}");
            }
        }
    }

    /// Requests recorded from several threads while another scrapes: no
    /// count is lost, every route is listed once, and no scrape shows a
    /// histogram whose buckets fall or pass its count.
    #[test]
    fn requests_recorded_beside_a_scrape_are_all_counted_and_each_scrape_is_consistent() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 5_000;
        const ROUTES: [&str; 4] = ["/a", "/b", "/c/{id}", "/d"];
        // One under a bound, one on a bound, one over every bound.
        const MICROS: [u64; 3] = [1, 5_000, 400_000_000];
        let series = |text: &str, name: &str, route: &str, le: Option<&str>| -> Option<u64> {
            let le = le.map(|le| format!(",le=\"{le}\"")).unwrap_or_default();
            value(
                text,
                &format!("mount_rs_http_request_duration_seconds_{name}{{route=\"{route}\"{le}}}"),
            )
            .map(|count| count.parse().expect("a count"))
        };
        let metrics = HttpMetrics::new("v");
        let done = std::sync::atomic::AtomicBool::new(false);
        let scrapes = std::thread::scope(|scope| {
            let scraper = scope.spawn(|| {
                let mut scrapes = 0_u32;
                loop {
                    // Read before the scrape, so the last one is of the
                    // finished counts.
                    let last = done.load(Ordering::Acquire);
                    let text = metrics.render();
                    for route in ROUTES {
                        let Some(infinity) = series(&text, "bucket", route, Some("+Inf")) else {
                            // Not counted yet.
                            continue;
                        };
                        let mut before = 0;
                        for (bound, _) in BUCKETS {
                            let count =
                                series(&text, "bucket", route, Some(bound)).expect("a bucket");
                            assert!(count >= before, "{route} le={bound}: {text}");
                            before = count;
                        }
                        assert!(infinity >= before, "{route}: {text}");
                        assert_eq!(
                            series(&text, "count", route, None),
                            Some(infinity),
                            "{route}: {text}"
                        );
                    }
                    scrapes += 1;
                    if last {
                        return scrapes;
                    }
                }
            });
            let recorders: Vec<_> = (0..THREADS)
                .map(|thread| {
                    let metrics = &metrics;
                    scope.spawn(move || {
                        for round in 0..ROUNDS {
                            let turn = thread + round;
                            record(
                                metrics,
                                if turn % 2 == 0 { "GET" } else { "PUT" },
                                ROUTES[turn % ROUTES.len()],
                                Some(200),
                                Duration::from_micros(MICROS[round % MICROS.len()]),
                                (1, 2),
                            );
                        }
                    })
                })
                .collect();
            for recorder in recorders {
                recorder.join().expect("a recorder");
            }
            done.store(true, Ordering::Release);
            scraper.join().expect("the scraper")
        });
        assert!(scrapes >= 1);

        let text = metrics.render();
        let total = (THREADS * ROUNDS) as u64;
        let samples = |prefix: &str| -> Vec<f64> {
            text.lines()
                .filter(|line| line.starts_with(prefix))
                .map(|line| {
                    let (_, sample) = line.rsplit_once(' ').expect("a sample");
                    sample.parse::<f64>().expect("a number")
                })
                .collect()
        };
        let requests: f64 = samples("mount_rs_http_requests_total{").iter().sum();
        assert_eq!(requests as u64, total, "{text}");
        // Each route's histogram is written once, however many first
        // requests for it raced, and between them they hold every request.
        let counts = samples("mount_rs_http_request_duration_seconds_count{");
        assert_eq!(counts.len(), ROUTES.len(), "{text}");
        assert_eq!(counts.iter().sum::<f64>() as u64, total, "{text}");
        let mut over_every_bound = 0;
        for route in ROUTES {
            let count = series(&text, "count", route, None).expect("a count");
            assert_eq!(series(&text, "bucket", route, Some("+Inf")), Some(count));
            over_every_bound += count - series(&text, "bucket", route, Some("300")).expect("300");
        }
        let rounds_over = (0..ROUNDS)
            .filter(|round| round % MICROS.len() == 2)
            .count();
        assert_eq!(over_every_bound, (rounds_over * THREADS) as u64, "{text}");
        // Every round's duration is known, so the sum is exact too.
        let per_thread: u64 = (0..ROUNDS).map(|round| MICROS[round % MICROS.len()]).sum();
        let micros = per_thread * THREADS as u64;
        let seconds: f64 = samples("mount_rs_http_request_duration_seconds_sum{")
            .iter()
            .sum();
        assert!(
            (seconds - micros as f64 / 1e6).abs() < 0.001,
            "{seconds} against {micros} microseconds"
        );
        assert_eq!(
            value(&text, "mount_rs_http_request_body_bytes_total"),
            Some(total.to_string())
        );
        assert_eq!(
            value(&text, "mount_rs_http_requests_in_flight").as_deref(),
            Some("0")
        );
    }

    #[tokio::test]
    async fn the_listener_refuses_an_address_that_is_not_loopback_unless_allowed() {
        let anywhere: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let refused = MetricsServer::start(anywhere, false, HttpMetrics::new("v"))
            .await
            .expect_err("not loopback");
        assert!(
            matches!(refused, HttpServerError::InsecureBind { .. }),
            "{refused}"
        );
        let allowed = MetricsServer::start(anywhere, true, HttpMetrics::new("v"))
            .await
            .expect("allowed");
        assert_ne!(allowed.local_addr().port(), 0);
        for loopback in ["127.0.0.1:0", "[::1]:0"] {
            let listen: SocketAddr = loopback.parse().unwrap();
            // A host without IPv6 cannot bind the second; that is not a
            // refusal.
            match MetricsServer::start(listen, false, HttpMetrics::new("v")).await {
                Ok(_) | Err(HttpServerError::Bind(_)) => {}
                Err(other) => panic!("{loopback}: {other}"),
            }
        }
    }
}
