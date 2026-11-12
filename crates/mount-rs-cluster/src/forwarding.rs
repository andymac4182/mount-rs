//! Server-side Redis discovery and bounded, single-hop HTTP/3 forwarding.
use crate::forward_trace::{ForwardRequestTrace, Stage};
use crate::request_class::RequestClass;
use crate::{
    HOP_HEADER, ROUTE_HEADER, RedisRouteRegistry, RegistryError, VolumeRoute, identifier,
    transport::*,
};
use futures_util::FutureExt;
use http::{HeaderMap, Request, Response, Uri};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Body;
use hyper::body::Frame;
use mount_rs_http::{BoxError, HttpBody};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    task::JoinHandle,
    time::{Instant, timeout_at},
};

/// Operator-authenticated membership. Never construct this from an unverified
/// Redis route; the endpoint and certificate pin must have independent trust.
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedPeer {
    pub node: String,
    pub boot: String,
    pub address: std::net::SocketAddr,
    pub tls_server_name: String,
    #[serde(rename = "spki_sha256", deserialize_with = "deserialize_peer_pin")]
    pub pin: SpkiSha256,
}

fn deserialize_peer_pin<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<SpkiSha256, D::Error> {
    use serde::de::Error;
    let text = <String as serde::Deserialize>::deserialize(input)?;
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(D::Error::custom(
            "spki_sha256 requires 64 hexadecimal digits",
        ));
    }
    let mut pin = [0; 32];
    for (byte, pair) in pin.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).map_err(D::Error::custom)?, 16)
            .map_err(D::Error::custom)?;
    }
    Ok(SpkiSha256::from_bytes(pin))
}

pub struct PeerDirectory(BTreeMap<String, TrustedPeer>);
impl PeerDirectory {
    /// Load an operator-provided JSON array of trusted peer records before binding listeners.
    pub fn from_reader(reader: impl std::io::Read) -> Result<Self, RegistryError> {
        let peers = serde_json::from_reader(reader).map_err(|_| RegistryError::InvalidInput)?;
        Self::new(peers)
    }

    pub fn new(peers: Vec<TrustedPeer>) -> Result<Self, RegistryError> {
        if peers.is_empty() || peers.len() > 64 {
            return Err(RegistryError::InvalidInput);
        }
        let mut map = BTreeMap::new();
        for peer in peers {
            if !identifier(&peer.node)
                || !identifier(&peer.boot)
                || !identifier(&peer.tls_server_name)
                || peer.address.port() == 0
                || map.contains_key(&peer.node)
            {
                return Err(RegistryError::InvalidInput);
            }
            map.insert(peer.node.clone(), peer);
        }
        Ok(Self(map))
    }
    /// Use the same operator-provided membership for TLS pins and node identity checks.
    pub fn approved_pins(&self) -> Result<ApprovedPeerPins, RegistryError> {
        ApprovedPeerPins::new(self.0.values().map(|peer| peer.pin))
            .map_err(|_| RegistryError::InvalidInput)
    }

    /// Authorize an incoming mTLS peer against its configured node and public key.
    pub fn accepts(&self, identity: &TlsIdentity) -> bool {
        self.authenticated_member(identity).is_some()
    }

    /// Identify the local membership record from the validated peer certificate.
    pub fn authenticated_member(&self, identity: &TlsIdentity) -> Option<&TrustedPeer> {
        let CertificateRole::Peer { id } = identity.role() else {
            return None;
        };
        self.0
            .get(id)
            .filter(|peer| peer.pin == identity.spki_sha256())
    }
    fn resolve(&self, route: &VolumeRoute) -> Option<&TrustedPeer> {
        self.0.get(&route.node).filter(|peer| {
            peer.boot == route.boot
                && peer.address == route.address
                && peer.tls_server_name == route.tls_server_name
        })
    }
}

#[derive(Clone, Copy)]
pub struct ForwardOptions {
    pub max_cached_routes: usize,
    pub route_cache_ttl: Duration,
    /// Experimental sliding advisory-route expiry on successful trusted-peer
    /// headers. Does not refresh ownership leases or authorize write replay.
    pub renew_route_on_success: bool,
    /// Confirm active cached routes with Redis ahead of their hard expiry.
    pub refresh_active_routes: bool,
    /// Stop proactive refresh after this many route TTLs without foreground
    /// use. Hard route expiry and the exact Redis record check are unchanged.
    pub refresh_idle_ttls: u32,
    pub operation_timeout: Duration,
    pub max_request_bytes: usize,
}
impl Default for ForwardOptions {
    fn default() -> Self {
        Self {
            max_cached_routes: 1024,
            route_cache_ttl: Duration::from_secs(1),
            renew_route_on_success: false,
            refresh_active_routes: false,
            refresh_idle_ttls: 5,
            operation_timeout: Duration::from_secs(30),
            max_request_bytes: 8 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardError {
    InvalidRequest,
    RouteUnavailable,
    UntrustedRoute,
    /// Headers or body might have reached the owner. Never replay a write based
    /// on this error; reconciliation requires a durable operation protocol.
    OutcomeUnknown,
}
impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "forwarding: {self:?}")
    }
}
impl std::error::Error for ForwardError {}

/// Completed-stage totals, not end-to-end percentiles. Incomplete stages are
/// excluded. Snapshot differences are meaningful while requests are quiescent.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct ForwardStageTiming {
    pub completed: u64,
    pub nanoseconds: u64,
    /// Inclusive cumulative thresholds; subtract snapshots only while quiescent.
    pub at_least_1ms: u64,
    pub at_least_5ms: u64,
    pub at_least_10ms: u64,
}
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct ForwardTimings {
    /// All route-cache hits, including hits after lookup coordination.
    /// Includes attempts whose later stages fail or cancel.
    pub route_cache_hits: u64,
    /// Subset of hits found only after acquiring per-volume lookup admission.
    /// Subtract from total hits for immediate snapshot-cache hits.
    pub route_cache_coalesced_hits: u64,
    pub route_cache_misses: u64,
    pub route: ForwardStageTiming,
    /// Registry and per-volume admission waiting, excluding this caller's Redis
    /// lookup. A substage of route; cancelled admission attempts are excluded.
    pub route_lookup_wait: ForwardStageTiming,
    /// Lookup-table acquisition plus pruning/entry selection, before per-volume
    /// admission. May complete even when later per-volume admission is cancelled.
    pub route_registry_admission: ForwardStageTiming,
    /// Per-volume discovery gate waiting only; excludes registry work and Redis
    /// I/O performed by this caller. Completed admissions only.
    pub route_volume_wait: ForwardStageTiming,
    /// Substages of `open`; do not add these to the total open duration.
    pub connection: ForwardStageTiming,
    pub stream_open: ForwardStageTiming,
    pub open: ForwardStageTiming,
    pub headers: ForwardStageTiming,
    /// Headers to the first nonempty payload handed to the downstream consumer.
    pub first_payload: ForwardStageTiming,
    /// Headers to the declared final payload, or EOF for unknown-length bodies.
    /// Includes consumer polling delays, not delivery to the remote client.
    pub body: ForwardStageTiming,
}
#[derive(Default)]
pub(crate) struct StageCounter {
    completed: AtomicU64,
    nanoseconds: AtomicU64,
    at_least_1ms: AtomicU64,
    at_least_5ms: AtomicU64,
    at_least_10ms: AtomicU64,
}
impl StageCounter {
    pub(crate) fn record(&self, start: Instant) {
        self.record_duration(start.elapsed());
    }
    pub(crate) fn record_duration(&self, elapsed: Duration) {
        for (threshold, counter) in [
            (1, &self.at_least_1ms),
            (5, &self.at_least_5ms),
            (10, &self.at_least_10ms),
        ] {
            if elapsed >= Duration::from_millis(threshold) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.nanoseconds.fetch_add(
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.completed.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn snapshot(&self) -> ForwardStageTiming {
        ForwardStageTiming {
            completed: self.completed.load(Ordering::Relaxed),
            nanoseconds: self.nanoseconds.load(Ordering::Relaxed),
            at_least_1ms: self.at_least_1ms.load(Ordering::Relaxed),
            at_least_5ms: self.at_least_5ms.load(Ordering::Relaxed),
            at_least_10ms: self.at_least_10ms.load(Ordering::Relaxed),
        }
    }
}
#[derive(Default)]
struct TimingCounters {
    route_cache_hits: AtomicU64,
    route_cache_coalesced_hits: AtomicU64,
    route_cache_misses: AtomicU64,
    route: StageCounter,
    route_lookup_wait: StageCounter,
    route_registry_admission: StageCounter,
    route_volume_wait: StageCounter,
    connection: StageCounter,
    stream_open: StageCounter,
    open: StageCounter,
    headers: StageCounter,
    first_payload: StageCounter,
    body: StageCounter,
}
impl TimingCounters {
    fn snapshot(&self) -> ForwardTimings {
        ForwardTimings {
            route_cache_hits: self.route_cache_hits.load(Ordering::Relaxed),
            route_cache_coalesced_hits: self.route_cache_coalesced_hits.load(Ordering::Relaxed),
            route_cache_misses: self.route_cache_misses.load(Ordering::Relaxed),
            route: self.route.snapshot(),
            route_lookup_wait: self.route_lookup_wait.snapshot(),
            route_registry_admission: self.route_registry_admission.snapshot(),
            route_volume_wait: self.route_volume_wait.snapshot(),
            connection: self.connection.snapshot(),
            stream_open: self.stream_open.snapshot(),
            open: self.open.snapshot(),
            headers: self.headers.snapshot(),
            first_payload: self.first_payload.snapshot(),
            body: self.body.snapshot(),
        }
    }
}

/// Cumulative body failures across clones of one forwarder. No request data or
/// transport-provided error messages are retained. These never authorize retry.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct ForwardDiagnostics {
    pub reads: ForwardTimings,
    pub mutations: ForwardTimings,
    /// Session lifecycle and handle RPC, including reads, writes and cleanup.
    pub session_rpc: ForwardTimings,
    pub ingress_route_unavailable: u64,
    pub ingress_route_untrusted: u64,
    pub background_route_lookups: u64,
    pub peer_not_dispatched_503: u64,
    pub response_receive_errors: u64,
    pub response_length_errors: u64,
    pub upload_completion_errors: u64,
}
#[derive(Default)]
struct BodyFailures {
    reads: TimingCounters,
    mutations: TimingCounters,
    session_rpc: TimingCounters,
    ingress_route_unavailable: AtomicU64,
    ingress_route_untrusted: AtomicU64,
    background_route_lookups: AtomicU64,
    peer_not_dispatched_503: AtomicU64,
    response_receive_errors: AtomicU64,
    response_length_errors: AtomicU64,
    upload_completion_errors: AtomicU64,
}

impl BodyFailures {
    fn timings(&self, class: RequestClass) -> &TimingCounters {
        match class {
            RequestClass::Read => &self.reads,
            RequestClass::Mutation => &self.mutations,
            RequestClass::SessionRpc => &self.session_rpc,
        }
    }
}

#[derive(Clone)]
pub struct IngressForwarder {
    registry: RedisRouteRegistry,
    peers: Arc<PeerDirectory>,
    client: Arc<PeerClient>,
    options: ForwardOptions,
    routes: Arc<crate::route_cache::RouteCache>,
    route_lookups: Arc<crate::coalesce::Coalesce>,
    body_failures: Arc<BodyFailures>,
}
impl IngressForwarder {
    pub fn new(
        registry: RedisRouteRegistry,
        peers: Arc<PeerDirectory>,
        client: Arc<PeerClient>,
        options: ForwardOptions,
    ) -> Result<Self, RegistryError> {
        if options.max_cached_routes == 0
            || options.max_cached_routes > 65536
            || options.route_cache_ttl.is_zero()
            || options.route_cache_ttl > Duration::from_secs(5)
            || !(1..=60).contains(&options.refresh_idle_ttls)
            || options.operation_timeout.is_zero()
            || options.operation_timeout > Duration::from_secs(300)
            || options.max_request_bytes == 0
            || options.max_request_bytes > 64 * 1024 * 1024
        {
            return Err(RegistryError::InvalidInput);
        }
        Ok(Self {
            registry,
            peers,
            client,
            options,
            routes: Arc::default(),
            route_lookups: Arc::default(),
            body_failures: Arc::default(),
        })
    }
    pub fn diagnostics(&self) -> ForwardDiagnostics {
        ForwardDiagnostics {
            reads: self.body_failures.reads.snapshot(),
            mutations: self.body_failures.mutations.snapshot(),
            session_rpc: self.body_failures.session_rpc.snapshot(),
            ingress_route_unavailable: self
                .body_failures
                .ingress_route_unavailable
                .load(Ordering::Relaxed),
            ingress_route_untrusted: self
                .body_failures
                .ingress_route_untrusted
                .load(Ordering::Relaxed),
            background_route_lookups: self
                .body_failures
                .background_route_lookups
                .load(Ordering::Relaxed),
            peer_not_dispatched_503: self
                .body_failures
                .peer_not_dispatched_503
                .load(Ordering::Relaxed),
            response_receive_errors: self
                .body_failures
                .response_receive_errors
                .load(Ordering::Relaxed),
            response_length_errors: self
                .body_failures
                .response_length_errors
                .load(Ordering::Relaxed),
            upload_completion_errors: self
                .body_failures
                .upload_completion_errors
                .load(Ordering::Relaxed),
        }
    }
    async fn route(
        &self,
        volume: &str,
        timings: &TimingCounters,
        trace: Option<&ForwardRequestTrace>,
    ) -> Result<Arc<crate::route_cache::CachedRoute>, ForwardError> {
        let started = Instant::now();
        let cached = if self.options.refresh_active_routes {
            self.routes.get_active(volume, started)
        } else {
            self.routes.get(volume, started)
        };
        if let Some(route) = cached {
            timings.route_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(route);
        }
        // Coalesce concurrent discovery for one volume. Cancellation drops
        // the caller's turn and its share; neither is held by a lock.
        let lookup_started = Instant::now();
        let lookup = self.route_lookups.join(volume);
        if let Some(trace) = trace {
            trace.record(Stage::LookupMapAdmission, lookup_started);
        }
        timings.route_registry_admission.record(lookup_started);
        let volume_wait_started = Instant::now();
        let _turn = match &lookup {
            None => None,
            Some(lookup) => Some(if let Some(trace) = trace {
                let (turn, elapsed) =
                    mount_rs_core::wait_timing::observe(lookup.turn().lock()).await;
                trace.record(Stage::VolumeAdmission, volume_wait_started);
                trace.interval(Stage::VolumePending, elapsed.pending_ns);
                trace.interval(Stage::VolumeNotified, elapsed.notified_ns);
                turn
            } else {
                lookup.turn().lock().await
            }),
        };
        timings.route_volume_wait.record(volume_wait_started);
        timings.route_lookup_wait.record(lookup_started);
        if let Some(trace) = trace {
            trace.record(Stage::RouteWait, lookup_started);
        }
        if let Some(route) = self.routes.get(volume, Instant::now()) {
            timings.route_cache_hits.fetch_add(1, Ordering::Relaxed);
            timings
                .route_cache_coalesced_hits
                .fetch_add(1, Ordering::Relaxed);
            return Ok(route);
        }
        timings.route_cache_misses.fetch_add(1, Ordering::Relaxed);
        let lookup_start = trace.map(|_| Instant::now());
        let result = self.registry.lookup(volume).await;
        if let (Some(trace), Some(start)) = (trace, lookup_start) {
            trace.record(Stage::RegistryLookup, start);
        }
        let route = result
            .map_err(|_| ForwardError::RouteUnavailable)?
            .ok_or(ForwardError::RouteUnavailable)?;
        if self.peers.resolve(&route).is_none() {
            return Err(ForwardError::UntrustedRoute);
        }
        let entry = self.routes.insert(
            &route,
            started + self.options.route_cache_ttl,
            self.options.max_cached_routes,
        );
        if self.options.refresh_active_routes {
            self.schedule_refresh(&route, &entry);
        }
        Ok(entry)
    }
    fn schedule_refresh(&self, route: &VolumeRoute, entry: &Arc<crate::route_cache::CachedRoute>) {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hash = DefaultHasher::new();
        route.volume.hash(&mut hash);
        let ttl = self.options.route_cache_ttl;
        let idle = ttl * self.options.refresh_idle_ttls;
        let interval = ttl.mul_f64(0.6 + (hash.finish() % 201) as f64 / 1000.0);
        let entry = Arc::downgrade(entry);
        let cache = Arc::downgrade(&self.routes);
        let registry = self.registry.clone();
        let counters = self.body_failures.clone();
        let (Ok(key), Ok(expected)) = (registry.key(&route.volume), route.encode()) else {
            return;
        };
        let route = route.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let (Some(entry), Some(cache)) = (entry.upgrade(), cache.upgrade()) else {
                    return;
                };
                let started = Instant::now();
                if !entry.can_refresh(started, idle) {
                    return;
                }
                counters
                    .background_route_lookups
                    .fetch_add(1, Ordering::Relaxed);
                match registry.matches_record(&key, expected.as_bytes()).await {
                    Ok(true) => {
                        if !entry.confirm(started, ttl) {
                            return;
                        }
                    }
                    Ok(false) => {
                        cache.discard_entry(&route, &entry);
                        return;
                    }
                    Err(_) => return,
                }
            }
        });
    }
    async fn invalidate(&self, route: &VolumeRoute) {
        self.routes.invalidate(route);
    }
    fn failed_route(&self, route: &VolumeRoute) -> ForwardError {
        // Evict only this exact advisory entry for subsequent independent calls.
        // The failed operation remains uncertain and must never be replayed here.
        self.routes.invalidate(route);
        ForwardError::OutcomeUnknown
    }
    /// Forward once. The owner checks the original drive bearer token. No route
    /// redirect, private endpoint or Redis credential is sent to the CLI client.
    /// The caller must serve the returned streaming body or drop it to cancel.
    pub async fn forward(
        &self,
        request: Request<HttpBody>,
    ) -> Result<Response<HttpBody>, ForwardError> {
        let trace = request.extensions().get::<ForwardRequestTrace>().cloned();
        let forward_start = Instant::now();
        let is_head = request.method() == http::Method::HEAD;
        let request_class = RequestClass::classify(&request);
        let timings = self.body_failures.timings(request_class);
        let deadline = forward_start + self.options.operation_timeout;
        if request.headers().contains_key(ROUTE_HEADER)
            || request.headers().contains_key(HOP_HEADER)
        {
            return Err(ForwardError::InvalidRequest);
        }
        if let Some(length) = request.headers().get("content-length") {
            let length = length
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(ForwardError::InvalidRequest)?;
            if length > self.options.max_request_bytes as u64 {
                return Err(ForwardError::InvalidRequest);
            }
        }
        let client_version = request.version();
        let path = request.uri().path();
        let volume = path
            .strip_prefix("/v1/drives/")
            .and_then(|path| path.split_once('/'))
            .map(|(volume, _)| volume)
            .filter(|volume| identifier(volume))
            .ok_or(ForwardError::InvalidRequest)?;
        let route_start = Instant::now();
        let entry = timeout_at(deadline, self.route(volume, timings, trace.as_ref()))
            .await
            .map_err(|_| ForwardError::RouteUnavailable)??;
        timings.route.record(route_start);
        if let Some(trace) = &trace {
            trace.record(Stage::Route, route_start);
        }
        let route = &entry.route;
        let peer = self
            .peers
            .resolve(route)
            .ok_or(ForwardError::UntrustedRoute)?;
        let (mut parts, mut body) = request.into_parts();
        strip_connection_headers(&mut parts.headers);
        parts.headers.remove("host");
        parts.headers.remove("x-mount-peer-timing");
        if trace.is_some() {
            parts
                .headers
                .insert("x-mount-peer-timing", http::HeaderValue::from_static("1"));
        }
        parts.headers.insert(
            ROUTE_HEADER,
            entry.header.clone().ok_or(ForwardError::InvalidRequest)?,
        );
        parts
            .headers
            .insert(HOP_HEADER, http::HeaderValue::from_static("1"));
        let path_query = parts
            .uri
            .path_and_query()
            .ok_or(ForwardError::InvalidRequest)?
            .as_str();
        parts.uri = Uri::builder()
            .scheme("https")
            .authority(route.tls_server_name.as_str())
            .path_and_query(path_query)
            .build()
            .map_err(|_| ForwardError::InvalidRequest)?;
        parts.version = http::Version::HTTP_3;
        let expected_pin = peer.pin;
        let expected_route = entry.clone();
        let policy = move |identity: &TlsIdentity, _: &Request<()>| {
            identity.spki_sha256() == expected_pin
                && matches!(identity.role(), CertificateRole::Peer { id } if id == &expected_route.route.node)
        };
        let open_start = Instant::now();
        let connection = timeout_at(
            deadline,
            self.client.connect(PeerDestination::new(
                route.address,
                route.tls_server_name.clone(),
            )),
        )
        .await
        .map_err(|_| self.failed_route(route))?
        .map_err(|_| self.failed_route(route))?;
        timings.connection.record(open_start);
        if let Some(trace) = &trace {
            trace.record(Stage::Connection, open_start);
        }
        let stream_start = Instant::now();
        let stream = timeout_at(
            deadline,
            connection.open_with_policy(Request::from_parts(parts, ()), &policy),
        )
        .await
        .map_err(|_| self.failed_route(route))?
        .map_err(|_| self.failed_route(route))?;
        timings.stream_open.record(stream_start);
        if let Some(trace) = &trace {
            trace.record(Stage::Stream, stream_start);
            let (connection, stream_id) = stream.diagnostic_ids();
            trace.interval(Stage::PeerConnectionId, connection);
            trace.interval(Stage::PeerStreamId, stream_id);
        }
        timings.open.record(open_start);
        let (send, recv) = stream.split();
        let maximum = self.options.max_request_bytes;
        let send = CancelSend(Some(send));
        let empty = body.is_end_stream();
        let transfer = async move {
            let mut send = send;
            let mut count = 0usize;
            while let Some(frame) = timeout_at(deadline, body.frame())
                .await
                .map_err(|_| ForwardError::OutcomeUnknown)?
            {
                let frame = frame.map_err(|_| ForwardError::OutcomeUnknown)?;
                if let Ok(bytes) = frame.into_data() {
                    count = count
                        .checked_add(bytes.len())
                        .filter(|n| *n <= maximum)
                        .ok_or(ForwardError::OutcomeUnknown)?;
                    send.0
                        .as_mut()
                        .unwrap()
                        .send_chunked_until(&bytes, deadline)
                        .await
                        .map_err(|_| ForwardError::OutcomeUnknown)?;
                }
            }
            timeout_at(deadline, send.0.as_mut().unwrap().finish())
                .await
                .map_err(|_| ForwardError::OutcomeUnknown)?
                .map_err(|_| ForwardError::OutcomeUnknown)?;
            send.0.take();
            Ok(())
        };
        // Empty requests normally finish on the first poll. If FIN is pending,
        // keep receiving response headers concurrently, as for a streaming upload.
        let upload = if empty {
            let mut transfer = Box::pin(transfer);
            match transfer.as_mut().now_or_never() {
                Some(result) => Upload::Complete(result.is_ok()),
                None => Upload::Running(AbortUpload(tokio::spawn(transfer))),
            }
        } else {
            Upload::Running(AbortUpload(tokio::spawn(transfer)))
        };
        let mut state = ResponseState {
            failure_route: None,
            trace,
            forward_start,
            request_class,
            body_start: Instant::now(),
            received_bytes: 0,
            expected_bytes: None,
            body_recorded: false,
            first_payload_recorded: false,
            finished: false,
            recv: CancelRecv(recv),
            upload,
            deadline,
            require_upload: true,
            body_failures: self.body_failures.clone(),
        };
        let headers_start = Instant::now();
        let receive = timeout_at(deadline, state.recv.0.recv_response());
        let response = if let Some(trace) = &state.trace {
            let (result, timing) = mount_rs_core::wait_timing::observe(receive).await;
            trace.interval(Stage::HeaderPending, timing.pending_ns);
            trace.interval(Stage::HeaderNotified, timing.notified_ns);
            result
        } else {
            receive.await
        }
        .map_err(|_| self.failed_route(route))?
        .map_err(|_| self.failed_route(route))?;
        timings.headers.record(headers_start);
        if let Some(trace) = &state.trace {
            trace.record(Stage::Headers, headers_start);
        }
        state.body_start = Instant::now();
        let (mut parts, _) = response.into_parts();
        for (header, stage) in [
            ("x-mount-owner-stream-queued-ns", Stage::OwnerStreamQueued),
            ("x-mount-owner-stream-resolve-ns", Stage::OwnerStreamResolve),
            ("x-mount-owner-admission-ns", Stage::OwnerAdmission),
            ("x-mount-owner-application-ns", Stage::OwnerApplication),
        ] {
            let value = parts.headers.remove(header);
            if let Some(trace) = &state.trace
                && let Some(ns) = value.and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
            {
                trace.interval(stage, ns);
            }
        }
        state.expected_bytes = expected_response_bytes(is_head, parts.status, &parts.headers);
        if parts
            .headers
            .get("x-mount-dispatch")
            .is_some_and(|value| value == "not-dispatched")
        {
            if parts.status == http::StatusCode::SERVICE_UNAVAILABLE {
                self.body_failures
                    .peer_not_dispatched_503
                    .fetch_add(1, Ordering::Relaxed);
            }
            self.invalidate(route).await;
        } else if self.options.renew_route_on_success && parts.status.is_success() {
            let now = Instant::now();
            // A delayed response may have been authorized before a handoff.
            // Bound its routing hint by the age of this request.
            self.routes
                .renew(route, route_start, now, self.options.route_cache_ttl);
        }
        // Transfer the existing route into the streaming state without cloning
        // its strings. Only observed peer response failures invalidate it.
        state.failure_route = Some((self.routes.clone(), entry));
        state.require_upload = parts.status.is_success();
        // Public HTTP writers may never poll a zero-length/HEAD response body.
        // Complete peer validation before handing those headers to the writer.
        if state.expected_bytes == Some(0) {
            state
                .verify_end(false)
                .await
                .map_err(|_| ForwardError::OutcomeUnknown)?;
        }
        strip_connection_headers(&mut parts.headers);
        parts.headers.remove(ROUTE_HEADER);
        parts.headers.remove(HOP_HEADER);
        let body = StreamBody::new(futures_util::stream::try_unfold(
            state,
            |mut state| async move {
                if state.finished {
                    return Ok(None);
                }
                match state.recv.0.recv_chunk_until(state.deadline).await {
                    Ok(Some(chunk)) => {
                        let bytes = chunk.into_bytes();
                        let first_payload = !bytes.is_empty() && !state.first_payload_recorded;
                        if first_payload {
                            if let Some(trace) = &state.trace {
                                trace.record(Stage::FirstPayloadReceived, state.body_start);
                            }
                            state
                                .body_failures
                                .timings(state.request_class)
                                .first_payload
                                .record(state.body_start);
                            state.first_payload_recorded = true;
                        }
                        state.received_bytes =
                            state.received_bytes.saturating_add(bytes.len() as u64);
                        state.check_length(false)?;
                        if !state.body_recorded
                            && state.expected_bytes == Some(state.received_bytes)
                        {
                            state
                                .body_failures
                                .timings(state.request_class)
                                .body
                                .record(state.body_start);
                            if let Some(trace) = &state.trace {
                                trace.record(Stage::Body, state.body_start);
                            }
                            state.body_recorded = true;
                        }
                        // A Content-Length writer can stop polling after this
                        // frame. Retain only the final chunk until peer EOF and
                        // successful-request upload completion are verified.
                        if state.expected_bytes == Some(state.received_bytes) {
                            state.verify_end(first_payload).await?;
                        } else if first_payload && let Some(trace) = &state.trace {
                            trace.record(Stage::FirstPayloadYielded, state.body_start);
                        }
                        Ok(Some((Frame::data(bytes), state)))
                    }
                    Ok(None) => {
                        state.finish(false).await?;
                        Ok(None)
                    }
                    Err(_) => {
                        state.invalidate_failed_response();
                        state
                            .body_failures
                            .response_receive_errors
                            .fetch_add(1, Ordering::Relaxed);
                        Err(Box::new(ForwardError::OutcomeUnknown) as BoxError)
                    }
                }
            },
        ))
        .boxed_unsync();
        parts.version = client_version;
        Ok(Response::from_parts(parts, body))
    }
}

fn strip_connection_headers(headers: &mut HeaderMap) {
    let nominated: Vec<String> = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(',').map(|part| part.trim().to_owned()))
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
    ] {
        headers.remove(name);
    }
}
struct CancelSend(Option<PeerClientSend>);
impl Drop for CancelSend {
    fn drop(&mut self) {
        if let Some(send) = self.0.as_mut() {
            send.cancel();
        }
    }
}
struct CancelRecv(PeerClientRecv);
impl Drop for CancelRecv {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct AbortUpload(JoinHandle<Result<(), ForwardError>>);
impl Drop for AbortUpload {
    fn drop(&mut self) {
        self.0.abort();
    }
}
enum Upload {
    Complete(bool),
    Running(AbortUpload),
}
struct ResponseState {
    failure_route: Option<(
        Arc<crate::route_cache::RouteCache>,
        Arc<crate::route_cache::CachedRoute>,
    )>,
    trace: Option<ForwardRequestTrace>,
    forward_start: Instant,
    request_class: RequestClass,
    body_start: Instant,
    received_bytes: u64,
    expected_bytes: Option<u64>,
    body_recorded: bool,
    first_payload_recorded: bool,
    finished: bool,
    body_failures: Arc<BodyFailures>,
    recv: CancelRecv,
    upload: Upload,
    deadline: Instant,
    require_upload: bool,
}

impl ResponseState {
    fn invalidate_failed_response(&self) {
        if let Some((routes, route)) = &self.failure_route {
            routes.invalidate(&route.route);
        }
    }

    async fn verify_end(&mut self, yielding_first_payload: bool) -> Result<(), BoxError> {
        let start = self.trace.as_ref().map(|_| Instant::now());
        loop {
            match self.recv.0.recv_chunk_until(self.deadline).await {
                Ok(Some(chunk)) => {
                    self.received_bytes = self
                        .received_bytes
                        .saturating_add(chunk.as_bytes().len() as u64);
                    self.check_length(false)?;
                }
                Ok(None) => {
                    if let (Some(trace), Some(start)) = (&self.trace, start) {
                        trace.record(Stage::VerifyEnd, start);
                    }
                    return self.finish(yielding_first_payload).await;
                }
                Err(_) => {
                    self.invalidate_failed_response();
                    self.body_failures
                        .response_receive_errors
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(Box::new(ForwardError::OutcomeUnknown));
                }
            }
        }
    }

    async fn finish(&mut self, yielding_first_payload: bool) -> Result<(), BoxError> {
        self.check_length(true)?;
        if self.require_upload {
            let start = self.trace.as_ref().map(|_| Instant::now());
            let completed = match &mut self.upload {
                Upload::Complete(ok) => *ok,
                Upload::Running(task) => {
                    matches!(timeout_at(self.deadline, &mut task.0).await, Ok(Ok(Ok(()))))
                }
            };
            if let (Some(trace), Some(start)) = (&self.trace, start) {
                trace.record(Stage::UploadJoin, start);
            }
            if !completed {
                self.body_failures
                    .upload_completion_errors
                    .fetch_add(1, Ordering::Relaxed);
                return Err(Box::new(ForwardError::OutcomeUnknown));
            }
        }
        if !self.body_recorded {
            self.body_failures
                .timings(self.request_class)
                .body
                .record(self.body_start);
        }
        if let Some(trace) = &self.trace {
            if !self.body_recorded {
                trace.record(Stage::Body, self.body_start);
            }
            if yielding_first_payload {
                trace.record(Stage::FirstPayloadYielded, self.body_start);
            }
            trace.finish(self.forward_start);
        }
        self.finished = true;
        Ok(())
    }

    fn check_length(&self, finished: bool) -> Result<(), BoxError> {
        if response_length_valid(self.expected_bytes, self.received_bytes, finished) {
            return Ok(());
        }
        self.invalidate_failed_response();
        self.body_failures
            .response_length_errors
            .fetch_add(1, Ordering::Relaxed);
        Err(Box::new(ForwardError::OutcomeUnknown))
    }
}

fn response_length_valid(expected: Option<u64>, received: u64, finished: bool) -> bool {
    expected.is_none_or(|length| {
        if finished {
            received == length
        } else {
            received <= length
        }
    })
}

fn expected_response_bytes(
    is_head: bool,
    status: http::StatusCode,
    headers: &HeaderMap,
) -> Option<u64> {
    if is_head || status == http::StatusCode::NO_CONTENT || status == http::StatusCode::NOT_MODIFIED
    {
        Some(0)
    } else {
        headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    }
}

impl mount_rs_http::HttpRequestHandler for IngressForwarder {
    fn handle(
        &self,
        request: Request<HttpBody>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response<HttpBody>> + Send + '_>> {
        Box::pin(async move {
            match self.forward(request).await {
                Ok(response) => response,
                Err(error) => {
                    let (status, certainty) = match error {
                        ForwardError::InvalidRequest => {
                            (http::StatusCode::BAD_REQUEST, "not-dispatched")
                        }
                        ForwardError::RouteUnavailable | ForwardError::UntrustedRoute => {
                            let counter = if matches!(error, ForwardError::UntrustedRoute) {
                                &self.body_failures.ingress_route_untrusted
                            } else {
                                &self.body_failures.ingress_route_unavailable
                            };
                            counter.fetch_add(1, Ordering::Relaxed);
                            (http::StatusCode::SERVICE_UNAVAILABLE, "not-dispatched")
                        }
                        ForwardError::OutcomeUnknown => (http::StatusCode::BAD_GATEWAY, "unknown"),
                    };
                    // No endpoint, credentials or provider diagnostics leave ingress.
                    let body = http_body_util::Full::new(hyper::body::Bytes::from_static(b"{\"error\":{\"code\":\"EIO\",\"message\":\"volume request unavailable\"}}"))
                        .map_err(|never| match never {}).boxed_unsync();
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .header("x-mount-dispatch", certainty)
                        .body(body)
                        .expect("static forwarding error response")
                }
            }
        })
    }
}

/// Public server application: serve owned volumes locally; discover and forward
/// other volumes internally. Clients use the same URL shape for both paths.
#[derive(Clone)]
pub struct ClusterIngress {
    local: crate::OwnerHttpRouter,
    remote: IngressForwarder,
}
impl ClusterIngress {
    pub fn new(local: crate::OwnerHttpRouter, remote: IngressForwarder) -> Self {
        Self { local, remote }
    }
}
impl mount_rs_http::HttpRequestHandler for ClusterIngress {
    fn handle(
        &self,
        request: Request<HttpBody>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response<HttpBody>> + Send + '_>> {
        Box::pin(async move {
            if !request.headers().contains_key(ROUTE_HEADER)
                && !request.headers().contains_key(HOP_HEADER)
            {
                let volume = request
                    .uri()
                    .path()
                    .strip_prefix("/v1/drives/")
                    .and_then(|path| path.split_once('/'))
                    .map(|(volume, _)| volume);
                if let Some(owner) = volume.and_then(|volume| self.local.local(volume)) {
                    return owner.local(request).await;
                }
            }
            mount_rs_http::HttpRequestHandler::handle(&self.remote, request).await
        })
    }
}

#[cfg(test)]
mod response_length_tests {
    use super::*;
    #[test]
    fn stage_thresholds_are_inclusive_and_support_snapshot_deltas() {
        let counter = StageCounter::default();
        for micros in [999, 1000, 4999, 5000, 9999, 10000] {
            counter.record_duration(Duration::from_micros(micros));
        }
        let before = counter.snapshot();
        assert_eq!(before.completed, 6);
        assert_eq!(before.nanoseconds, 31_997_000);
        assert_eq!(before.at_least_1ms, 5);
        assert_eq!(before.at_least_5ms, 3);
        assert_eq!(before.at_least_10ms, 1);
        counter.record_duration(Duration::from_millis(20));
        let after = counter.snapshot();
        assert_eq!(after.completed - before.completed, 1);
        assert_eq!(after.at_least_1ms - before.at_least_1ms, 1);
        assert_eq!(after.at_least_5ms - before.at_least_5ms, 1);
        assert_eq!(after.at_least_10ms - before.at_least_10ms, 1);
    }
    #[test]
    fn advertised_length_rejects_short_eof_and_extra_bytes() {
        assert!(response_length_valid(Some(4096), 2048, false));
        assert!(!response_length_valid(Some(4096), 2048, true));
        assert!(!response_length_valid(Some(4096), 4097, false));
        assert!(response_length_valid(Some(4096), 4096, true));
        assert!(response_length_valid(None, 2048, true));
    }
    #[test]
    fn head_and_bodyless_statuses_ignore_representation_length() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, "4096".parse().unwrap());
        assert_eq!(
            expected_response_bytes(true, http::StatusCode::OK, &headers),
            Some(0)
        );
        assert_eq!(
            expected_response_bytes(false, http::StatusCode::NOT_MODIFIED, &headers),
            Some(0)
        );
        assert_eq!(
            expected_response_bytes(false, http::StatusCode::NO_CONTENT, &headers),
            Some(0)
        );
        assert_eq!(
            expected_response_bytes(false, http::StatusCode::PARTIAL_CONTENT, &headers),
            Some(4096)
        );
    }
}

#[cfg(test)]
mod route_lookup_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One complete RESP command at the front of `buffer` and its length.
    fn command(buffer: &[u8]) -> Option<(Vec<Vec<u8>>, usize)> {
        fn line(buffer: &[u8], from: usize) -> Option<(&[u8], usize)> {
            let end = from + buffer[from..].windows(2).position(|w| w == b"\r\n")?;
            Some((&buffer[from..end], end + 2))
        }
        fn number(line: &[u8], prefix: u8) -> usize {
            assert_eq!(line[0], prefix);
            std::str::from_utf8(&line[1..]).unwrap().parse().unwrap()
        }
        let (head, mut at) = line(buffer, 0)?;
        let mut parts = Vec::new();
        for _ in 0..number(head, b'*') {
            let (length, start) = line(buffer, at)?;
            let end = start + number(length, b'$');
            parts.push(buffer.get(start..end)?.to_vec());
            buffer.get(end..end + 2)?;
            at = end + 2;
        }
        Some((parts, at))
    }

    /// A registry that counts route lookups and answers none of them until
    /// told, so every caller that is going to ask has asked.
    async fn registry(
        record: String,
        lookups: Arc<AtomicUsize>,
        answer: tokio::sync::watch::Receiver<bool>,
    ) -> RedisRouteRegistry {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut read, mut write) = socket.into_split();
            let (replies, mut queued) = tokio::sync::mpsc::unbounded_channel::<bool>();
            tokio::spawn(async move {
                let mut answer = answer;
                while let Some(lookup) = queued.recv().await {
                    let reply = if lookup {
                        answer.wait_for(|go| *go).await.unwrap();
                        format!("${}\r\n{record}\r\n", record.len())
                    } else {
                        "+OK\r\n".to_owned()
                    };
                    write.write_all(reply.as_bytes()).await.unwrap();
                }
            });
            let mut buffer = Vec::new();
            loop {
                while let Some((parts, used)) = command(&buffer) {
                    buffer.drain(..used);
                    let lookup = parts[0].eq_ignore_ascii_case(b"GETRANGE");
                    if lookup {
                        lookups.fetch_add(1, Ordering::SeqCst);
                    }
                    replies.send(lookup).unwrap();
                }
                if read.read_buf(&mut buffer).await.unwrap_or(0) == 0 {
                    return;
                }
            }
        });
        let connection = redis::Client::open(format!("redis://{address}/"))
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        RedisRouteRegistry::new(connection, "test".into(), Duration::from_secs(10)).unwrap()
    }

    #[tokio::test]
    async fn concurrent_requests_on_a_cold_route_make_one_registry_lookup() {
        const CALLERS: usize = 8;
        let peer = TrustedPeer {
            node: "owner".into(),
            boot: "boot".into(),
            address: "127.0.0.1:4443".parse().unwrap(),
            tls_server_name: "localhost".into(),
            pin: SpkiSha256::from_bytes([7; 32]),
        };
        let route = VolumeRoute {
            volume: "volume".into(),
            node: peer.node.clone(),
            boot: peer.boot.clone(),
            registration: "registration".into(),
            fence: 1,
            address: peer.address,
            tls_server_name: peer.tls_server_name.clone(),
        };
        let lookups = Arc::new(AtomicUsize::new(0));
        let (answer, held) = tokio::sync::watch::channel(false);
        let registry = registry(route.encode().unwrap(), lookups.clone(), held).await;
        // The client is never dialled: only route discovery runs.
        let client = PeerClient::new(
            quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap(),
            ApprovedPeerPins::new([peer.pin]).unwrap(),
            PeerTransportLimits::default(),
        )
        .unwrap();
        let forwarder = IngressForwarder::new(
            registry,
            Arc::new(PeerDirectory::new(vec![peer]).unwrap()),
            Arc::new(client),
            ForwardOptions {
                route_cache_ttl: Duration::from_secs(5),
                ..ForwardOptions::default()
            },
        )
        .unwrap();
        let timings = Arc::new(TimingCounters::default());
        let mut callers = tokio::task::JoinSet::new();
        for _ in 0..CALLERS {
            let forwarder = forwarder.clone();
            let timings = timings.clone();
            callers.spawn(async move {
                forwarder
                    .route("volume", &timings, None)
                    .await
                    .map(|entry| entry.route.clone())
            });
        }
        // The registry holds its answer, so a caller that does not wait for
        // the first one's lookup sends its own in this window.
        let window = std::time::Instant::now() + Duration::from_millis(150);
        while lookups.load(Ordering::SeqCst) < CALLERS && std::time::Instant::now() < window {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        answer.send(true).unwrap();
        while let Some(found) = callers.join_next().await {
            assert_eq!(found.unwrap().unwrap(), route);
        }
        assert_eq!(
            lookups.load(Ordering::SeqCst),
            1,
            "callers for one cold route each asked the registry"
        );
        assert_eq!(timings.route_cache_misses.load(Ordering::Relaxed), 1);
        assert_eq!(
            timings.route_cache_coalesced_hits.load(Ordering::Relaxed),
            CALLERS as u64 - 1
        );
    }
}
