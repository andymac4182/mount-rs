use super::http_observation::{ObservedConnector, ObservedService};
use super::*;
use async_trait::async_trait;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use mount_rs_core::diagnostics::object_store::HttpMethod;
use object_store::client::{
    HttpClient, HttpConnector, HttpError, HttpErrorKind, HttpRequest, HttpRequestBody,
    HttpResponse, HttpResponseBody, HttpService,
};
use object_store::path::Path;
use object_store::{ClientConfigKey, PutMode, PutOptions, PutPayload};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

#[derive(Debug, PartialEq, Eq)]
struct RequestObservation {
    offered: usize,
    signed: bool,
    conditional_create: bool,
}
struct Fixture {
    replies: Mutex<VecDeque<std::result::Result<HttpResponse, HttpError>>>,
    requests: Mutex<Vec<RequestObservation>>,
    pending: bool,
    entered: AtomicUsize,
    exited: AtomicUsize,
    options: Mutex<Vec<(Option<String>, Option<String>)>>,
}
impl Fixture {
    fn new(replies: Vec<std::result::Result<HttpResponse, HttpError>>, pending: bool) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::with_capacity(16)),
            pending,
            entered: AtomicUsize::new(0),
            exited: AtomicUsize::new(0),
            options: Mutex::new(Vec::with_capacity(4)),
        })
    }
}
struct ActualCallOwner(Arc<Fixture>);
impl Drop for ActualCallOwner {
    fn drop(&mut self) {
        self.0.exited.fetch_add(1, Ordering::SeqCst);
    }
}
struct FixtureService(Arc<Fixture>);
impl fmt::Debug for FixtureService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FixtureService")
    }
}
#[async_trait]
impl HttpService for FixtureService {
    async fn call(&self, request: HttpRequest) -> std::result::Result<HttpResponse, HttpError> {
        self.0.entered.fetch_add(1, Ordering::SeqCst);
        let _actual = ActualCallOwner(self.0.clone());
        self.0.requests.lock().unwrap().push(RequestObservation {
            offered: request.body().content_length(),
            signed: request
                .headers()
                .get("authorization")
                .is_some_and(|v| v.as_bytes().starts_with(b"AWS4-HMAC-SHA256 ")),
            conditional_create: request
                .headers()
                .get("if-none-match")
                .is_some_and(|v| v.as_bytes() == b"*"),
        });
        if self.0.pending {
            return std::future::pending().await;
        }
        self.0
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted HTTP reply")
    }
}
#[derive(Clone)]
struct FixtureConnector {
    fixture: Arc<Fixture>,
    fail: bool,
}
impl fmt::Debug for FixtureConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FixtureConnector")
    }
}
impl HttpConnector for FixtureConnector {
    fn connect(&self, options: &ClientOptions) -> object_store::Result<HttpClient> {
        self.fixture.options.lock().unwrap().push((
            options.get_config_value(&ClientConfigKey::Timeout),
            options.get_config_value(&ClientConfigKey::ConnectTimeout),
        ));
        if self.fail {
            return Err(object_store::Error::Generic {
                store: "private-unit-connector",
                source: std::io::Error::other("unit-construction-fault").into(),
            });
        }
        Ok(HttpClient::new(FixtureService(self.fixture.clone())))
    }
}
fn request(method: &str, data: Bytes) -> HttpRequest {
    let mut request = HttpRequest::new(HttpRequestBody::from(data));
    *request.method_mut() = method.parse().unwrap();
    *request.uri_mut() = "http://127.0.0.1:1/private-unit".parse().unwrap();
    request
}
fn reply(status: u16, body: HttpResponseBody) -> HttpResponse {
    let mut response = HttpResponse::new(body);
    *response.status_mut() = status.try_into().unwrap();
    response
        .headers_mut()
        .insert("etag", "\"unit\"".parse().unwrap());
    response
}
fn connector(fixture: &Arc<Fixture>) -> FixtureConnector {
    FixtureConnector {
        fixture: fixture.clone(),
        fail: false,
    }
}
fn observed(fixture: &Arc<Fixture>, observer: &Observer, role: ClientRole) -> HttpClient {
    ObservedConnector::new(connector(fixture), observer.clone(), role)
        .connect(&ClientOptions::new())
        .unwrap()
}
fn config() -> RustFsConfig {
    RustFsConfig {
        endpoint: "http://127.0.0.1:1".into(),
        bucket: "private-unit".into(),
        access_key_id: "unit-key".into(),
        secret_access_key: "unit-secret".into(),
        region: "us-east-1".into(),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "platform client construction profile"]
async fn native_http_client_build_profile() {
    let configuration = config();
    for sample_index in 0..3 {
        let started = std::time::Instant::now();
        let client = configuration
            .build_client_with_probe_limits(true)
            .expect("native probe client construction must succeed");
        let elapsed_ns = u64::try_from(started.elapsed().as_nanos())
            .expect("native client construction duration must fit u64 nanoseconds");
        std::hint::black_box(&client);
        drop(client);
        println!(
            "native_http_client_build_profile {{\"schema\":\"mount-rs.rustfs-native-http-client-build-profile.v1\",\"pid\":{},\"sample_index\":{},\"elapsed_ns\":{},\"probe\":true,\"actual_native_reqwest\":true,\"requests_performed\":0}}",
            std::process::id(),
            sample_index,
            elapsed_ns,
        );
    }
}

fn assert_connector_build_row(observer: &Observer, role: ClientRole, expected: [u64; 5]) {
    let snapshot = serde_json::to_value(observer.snapshot().unwrap()).unwrap();
    let build = &snapshot["clients"][role.index()]["build"];
    assert!(
        build.is_object(),
        "actual connector construction must export its per-role build observation"
    );
    assert_eq!(build.as_object().unwrap().len(), 7);
    for (field, expected) in ["started", "inflight", "succeeded", "failed", "abandoned"]
        .into_iter()
        .zip(expected)
    {
        assert_eq!(build[field].as_u64(), Some(expected), "{field}");
    }
    let elapsed = build["elapsed_ns"].as_u64().unwrap();
    let maximum = build["max_ns"].as_u64().unwrap();
    assert!(maximum <= elapsed);
}

#[test]
fn connector_build_success_and_typed_error_are_observed_without_http_dispatch() {
    let observer = Observer::isolated();
    let successful = Fixture::new(Vec::new(), false);
    let failed = Fixture::new(Vec::new(), false);
    let options = ClientOptions::new()
        .with_timeout(std::time::Duration::from_secs(8))
        .with_connect_timeout(std::time::Duration::from_secs(3));
    let expected_options = vec![(
        options.get_config_value(&ClientConfigKey::Timeout),
        options.get_config_value(&ClientConfigKey::ConnectTimeout),
    )];
    let client = ObservedConnector::new(
        connector(&successful),
        observer.clone(),
        ClientRole::StandaloneProbe,
    )
    .connect(&options)
    .expect("the actual fixture connector must return its original successful client");
    let error = ObservedConnector::new(
        FixtureConnector {
            fixture: failed.clone(),
            fail: true,
        },
        observer.clone(),
        ClientRole::QualificationProbe,
    )
    .connect(&options)
    .unwrap_err();
    match error {
        object_store::Error::Generic { store, source } => {
            assert_eq!(store, "private-unit-connector");
            assert_eq!(source.to_string(), "unit-construction-fault");
        }
        error => panic!("original fixture connector error type changed: {error:?}"),
    }
    assert_eq!(*successful.options.lock().unwrap(), expected_options);
    assert_eq!(*failed.options.lock().unwrap(), expected_options);
    assert_eq!(successful.entered.load(Ordering::SeqCst), 0);
    assert_eq!(failed.entered.load(Ordering::SeqCst), 0);
    let snapshot = observer.snapshot().unwrap();
    let success = snapshot.clients[ClientRole::StandaloneProbe.index()];
    let error = snapshot.clients[ClientRole::QualificationProbe.index()];
    assert_eq!(
        (success.constructed, success.released, success.live),
        (1, 0, 1)
    );
    assert_eq!((error.constructed, error.released, error.live), (0, 0, 0));
    assert!(
        snapshot
            .clients
            .iter()
            .all(|client| client.http.iter().all(|http| http.attempts_started == 0))
    );
    drop(client);
    let released = observer.snapshot().unwrap().clients[ClientRole::StandaloneProbe.index()];
    assert_eq!(
        (released.constructed, released.released, released.live),
        (1, 1, 0)
    );
    assert_connector_build_row(&observer, ClientRole::StandaloneProbe, [1, 0, 1, 0, 0]);
    assert_connector_build_row(&observer, ClientRole::QualificationProbe, [1, 0, 0, 1, 0]);
}

#[derive(Debug)]
struct BuildPanicConnector(FixtureConnector);
impl HttpConnector for BuildPanicConnector {
    fn connect(&self, options: &ClientOptions) -> object_store::Result<HttpClient> {
        let _actual_client = self.0.connect(options)?;
        panic!("fixed fixture connector construction panic");
    }
}

#[test]
fn connector_build_panic_unwind_is_abandoned_without_constructed_client_or_dispatch() {
    let observer = Observer::isolated();
    let fixture = Fixture::new(Vec::new(), false);
    let options = ClientOptions::new();
    let connector = ObservedConnector::new(
        BuildPanicConnector(connector(&fixture)),
        observer.clone(),
        ClientRole::StandaloneData,
    );
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| connector.connect(&options)));
    assert!(
        result.is_err(),
        "the actual fixture panic must propagate unchanged"
    );
    assert_eq!(fixture.options.lock().unwrap().len(), 1);
    assert_eq!(fixture.entered.load(Ordering::SeqCst), 0);
    let snapshot = observer.snapshot().unwrap();
    let row = snapshot.clients[ClientRole::StandaloneData.index()];
    assert_eq!((row.constructed, row.released, row.live), (0, 0, 0));
    assert!(
        snapshot
            .clients
            .iter()
            .all(|client| client.http.iter().all(|http| http.attempts_started == 0))
    );
    assert_connector_build_row(&observer, ClientRole::StandaloneData, [1, 0, 0, 0, 1]);
}

#[test]
fn connector_build_disabled_preserves_success_error_and_options_without_observation() {
    let observer = Observer::disabled();
    let fixture = Fixture::new(Vec::new(), false);
    let options = ClientOptions::new().with_timeout(std::time::Duration::from_secs(8));
    let expected_options = (
        options.get_config_value(&ClientConfigKey::Timeout),
        options.get_config_value(&ClientConfigKey::ConnectTimeout),
    );
    let client = ObservedConnector::new(
        connector(&fixture),
        observer.clone(),
        ClientRole::StandaloneProbe,
    )
    .connect(&options)
    .unwrap();
    drop(client);
    let error = ObservedConnector::new(
        FixtureConnector {
            fixture: fixture.clone(),
            fail: true,
        },
        observer.clone(),
        ClientRole::StandaloneProbe,
    )
    .connect(&options)
    .unwrap_err();
    match error {
        object_store::Error::Generic { store, source } => {
            assert_eq!(store, "private-unit-connector");
            assert_eq!(source.to_string(), "unit-construction-fault");
        }
        error => panic!("disabled fixture connector error type changed: {error:?}"),
    }
    assert_eq!(
        *fixture.options.lock().unwrap(),
        vec![expected_options.clone(), expected_options]
    );
    assert_eq!(fixture.entered.load(Ordering::SeqCst), 0);
    assert!(observer.snapshot().is_none());
}

fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}
fn poll_body(
    body: &mut HttpResponseBody,
) -> Poll<Option<std::result::Result<Frame<Bytes>, HttpError>>> {
    Pin::new(body).poll_frame(&mut Context::from_waker(Waker::noop()))
}

struct ScriptBody {
    frames: VecDeque<std::result::Result<Frame<Bytes>, HttpError>>,
    pending: bool,
    end_hint: bool,
    polls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}
impl Body for ScriptBody {
    type Data = Bytes;
    type Error = HttpError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, HttpError>>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if let Some(frame) = self.frames.pop_front() {
            return Poll::Ready(Some(frame));
        }
        if self.pending {
            Poll::Pending
        } else {
            Poll::Ready(None)
        }
    }
    fn is_end_stream(&self) -> bool {
        self.end_hint && self.frames.is_empty()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(73)
    }
}
impl Drop for ScriptBody {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn body(
    frames: Vec<std::result::Result<Frame<Bytes>, HttpError>>,
    pending: bool,
    end_hint: bool,
) -> (HttpResponseBody, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let polls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    (
        HttpResponseBody::new(ScriptBody {
            frames: frames.into(),
            pending,
            end_hint,
            polls: polls.clone(),
            drops: drops.clone(),
        }),
        polls,
        drops,
    )
}

#[tokio::test]
async fn actual_s3_retry_counts_two_dispatches_with_original_signing_and_create_headers() {
    let observer = Observer::isolated();
    let (retry_body, _, retry_drop) = body(Vec::new(), true, false);
    let fixture = Fixture::new(
        vec![
            Ok(reply(503, retry_body)),
            Ok(reply(200, Bytes::new().into())),
        ],
        false,
    );
    let store = config()
        .build_client_with_injected_connector(
            false,
            ClientRole::PrimaryDataMixed,
            &observer,
            connector(&fixture),
        )
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        store.put_opts(
            &Path::from("immutable-unit"),
            PutPayload::from(Bytes::from_static(b"unit-data")),
            PutOptions {
                mode: PutMode::Create,
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap();
    assert!(result.is_ok());
    assert_eq!(
        *fixture.requests.lock().unwrap(),
        vec![
            RequestObservation {
                offered: 9,
                signed: true,
                conditional_create: true
            },
            RequestObservation {
                offered: 9,
                signed: true,
                conditional_create: true
            },
        ]
    );
    assert_eq!(retry_drop.load(Ordering::SeqCst), 1);
    let s = observer.snapshot().unwrap();
    let row = &s.clients[ClientRole::PrimaryDataMixed.index()].http[HttpMethod::Put.index()];
    assert_eq!(row.attempts_started, 2);
    assert_eq!(row.header_responses, 2);
    assert_eq!(row.offered_bytes, 18);
    assert_eq!(row.transport_errors, 0);
    assert_eq!(row.status[4], 1);
    assert_eq!(row.status[1], 1);
    // Neither retry response nor successful empty PUT response is polled.
    assert_eq!(row.body_dropped, 2);
    assert_eq!(row.body_eof, 0);
    assert_eq!(row.cancelled_before_headers, 0);
    assert_eq!(row.known_extra_future_boxes, 2);
    assert_eq!(row.known_extra_response_body_boxes, 2);
}

#[test]
fn unpolled_observed_call_counts_box_construction_without_dispatch_or_cancellation() {
    let observer = Observer::isolated();
    let fixture = Fixture::new(Vec::new(), true);
    let service = ObservedService::new(
        HttpClient::new(FixtureService(fixture.clone())),
        observer.clone(),
        ClientRole::StandaloneData,
    );
    drop(service.call(request("GET", Bytes::new())));
    let s = observer.snapshot().unwrap();
    let row = &s.clients[ClientRole::StandaloneData.index()].http[HttpMethod::Get.index()];
    assert_eq!(row.known_extra_future_boxes, 1);
    assert_eq!(row.attempts_started, 0);
    assert_eq!(row.cancelled_before_headers, 0);
    assert_eq!(fixture.entered.load(Ordering::SeqCst), 0);
}

#[test]
fn held_actual_dispatch_drop_preserves_owner_drop_and_records_only_preheader_cancel() {
    let observer = Observer::isolated();
    let fixture = Fixture::new(Vec::new(), true);
    let service = ObservedService::new(
        HttpClient::new(FixtureService(fixture.clone())),
        observer.clone(),
        ClientRole::StandaloneProbe,
    );
    let mut future = service.call(request("HEAD", Bytes::new()));
    assert!(poll_once(future.as_mut()).is_pending());
    assert_eq!(fixture.entered.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.exited.load(Ordering::SeqCst), 0);
    let s = observer.snapshot().unwrap();
    assert_eq!(
        s.clients[ClientRole::StandaloneProbe.index()].http[HttpMethod::Head.index()]
            .attempts_inflight,
        1
    );
    drop(future);
    assert_eq!(fixture.exited.load(Ordering::SeqCst), 1);
    let s = observer.snapshot().unwrap();
    let row = &s.clients[ClientRole::StandaloneProbe.index()].http[HttpMethod::Head.index()];
    assert_eq!(row.cancelled_before_headers, 1);
    assert_eq!(row.attempts_inflight, 0);
    assert_eq!(row.header_responses, 0);
    assert_eq!(row.bodies_started, 0);
}

#[tokio::test]
async fn body_frames_trailers_pointer_sizehint_and_eof_pass_through_without_copy() {
    let observer = Observer::isolated();
    let bytes = Bytes::from_static(b"borrowed-data");
    let ptr = bytes.as_ptr();
    let mut trailers = HttpResponse::new(Bytes::new().into())
        .into_parts()
        .0
        .headers;
    trailers.insert("x-unit-trailer", "present".parse().unwrap());
    let (inner, polls, drops) = body(
        vec![Ok(Frame::data(bytes)), Ok(Frame::trailers(trailers))],
        false,
        false,
    );
    let fixture = Fixture::new(vec![Ok(reply(200, inner))], false);
    let client = observed(&fixture, &observer, ClientRole::StandaloneData);
    let mut response = client
        .execute(request("GET", Bytes::new()))
        .await
        .unwrap()
        .into_body();
    assert_eq!(response.size_hint().exact(), Some(73));
    assert!(!response.is_end_stream());
    match poll_body(&mut response) {
        Poll::Ready(Some(Ok(frame))) => {
            let data = frame.data_ref().unwrap();
            assert_eq!(data.as_ptr(), ptr);
            assert_eq!(data.as_ref(), b"borrowed-data");
        }
        _ => panic!("expected unchanged data frame"),
    }
    match poll_body(&mut response) {
        Poll::Ready(Some(Ok(frame))) => {
            assert_eq!(
                frame
                    .into_trailers()
                    .unwrap()
                    .get("x-unit-trailer")
                    .unwrap(),
                "present"
            );
        }
        _ => panic!("expected unchanged trailers frame"),
    }
    assert!(matches!(poll_body(&mut response), Poll::Ready(None)));
    assert!(matches!(poll_body(&mut response), Poll::Ready(None)));
    drop(response);
    assert_eq!(polls.load(Ordering::SeqCst), 4);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let s = observer.snapshot().unwrap();
    let row = &s.clients[ClientRole::StandaloneData.index()].http[HttpMethod::Get.index()];
    assert_eq!(row.body_bytes, 13);
    assert_eq!(row.body_chunks, 1);
    assert_eq!(row.body_eof, 1);
    assert_eq!(row.body_errors, 0);
    assert_eq!(row.body_dropped, 0);
    assert_eq!(row.bodies_inflight, 0);
}

#[tokio::test]
async fn typed_transport_and_body_errors_remain_distinct_and_body_prefix_is_counted() {
    let observer = Observer::isolated();
    let fixture = Fixture::new(
        vec![Err(HttpError::new(
            HttpErrorKind::Connect,
            std::io::Error::other("unit-transport-fault"),
        ))],
        false,
    );
    let client = observed(&fixture, &observer, ClientRole::StandaloneData);
    assert_eq!(
        client
            .execute(request("GET", Bytes::new()))
            .await
            .unwrap_err()
            .kind(),
        HttpErrorKind::Connect
    );
    let s = observer.snapshot().unwrap();
    let row = &s.clients[ClientRole::StandaloneData.index()].http[HttpMethod::Get.index()];
    assert_eq!(row.transport_errors, 1);
    assert_eq!(row.header_responses, 0);
    assert_eq!(row.bodies_started, 0);
    let (inner, _, drops) = body(
        vec![
            Ok(Frame::data(Bytes::from_static(b"prefix"))),
            Err(HttpError::new(
                HttpErrorKind::Decode,
                std::io::Error::other("unit-body-fault"),
            )),
        ],
        false,
        false,
    );
    let fixture = Fixture::new(vec![Ok(reply(200, inner))], false);
    let client = observed(&fixture, &observer, ClientRole::StandaloneData);
    let mut response = client
        .execute(request("GET", Bytes::new()))
        .await
        .unwrap()
        .into_body();
    assert!(matches!(poll_body(&mut response), Poll::Ready(Some(Ok(_)))));
    match poll_body(&mut response) {
        Poll::Ready(Some(Err(error))) => assert_eq!(error.kind(), HttpErrorKind::Decode),
        _ => panic!("expected original typed body error"),
    }
    drop(response);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let s = observer.snapshot().unwrap();
    let row = &s.clients[ClientRole::StandaloneData.index()].http[HttpMethod::Get.index()];
    assert_eq!(row.transport_errors, 1);
    assert_eq!(row.header_responses, 1);
    assert_eq!(row.body_errors, 1);
    assert_eq!(row.body_bytes, 6);
    assert_eq!(row.body_dropped, 0);
    assert_eq!(row.body_eof, 0);
}

#[tokio::test]
async fn pending_response_body_keeps_no_observed_service_and_drop_is_not_dispatch_cancel() {
    let observer = Observer::isolated();
    let (inner, polls, drops) = body(Vec::new(), true, false);
    let fixture = Fixture::new(vec![Ok(reply(200, inner))], false);
    let service = Arc::new(ObservedService::new(
        HttpClient::new(FixtureService(fixture.clone())),
        observer.clone(),
        ClientRole::StandaloneData,
    ));
    let weak = Arc::downgrade(&service);
    let mut response = service
        .call(request("GET", Bytes::new()))
        .await
        .unwrap()
        .into_body();
    drop(service);
    assert!(weak.upgrade().is_none());
    let s = observer.snapshot().unwrap();
    let client = &s.clients[ClientRole::StandaloneData.index()];
    assert_eq!(client.live, 0);
    assert_eq!(client.released, 1);
    assert_eq!(client.http[HttpMethod::Get.index()].bodies_inflight, 1);
    assert!(poll_body(&mut response).is_pending());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    drop(response);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let s = observer.snapshot().unwrap();
    let row = &s.clients[ClientRole::StandaloneData.index()].http[HttpMethod::Get.index()];
    assert_eq!(row.body_dropped, 1);
    assert_eq!(row.bodies_inflight, 0);
    assert_eq!(row.cancelled_before_headers, 0);
}

#[tokio::test]
async fn initially_empty_head_hint_does_not_infer_eof_and_actual_none_is_positive_control() {
    for method in ["HEAD", "PUT"] {
        let observer = Observer::isolated();
        let (inner, polls, drops) = body(Vec::new(), false, true);
        let fixture = Fixture::new(vec![Ok(reply(200, inner))], false);
        let client = observed(&fixture, &observer, ClientRole::StandaloneProbe);
        let response = client.execute(request(method, Bytes::new())).await.unwrap();
        assert!(response.body().is_end_stream());
        drop(response);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let s = observer.snapshot().unwrap();
        let index = if method == "HEAD" {
            HttpMethod::Head.index()
        } else {
            HttpMethod::Put.index()
        };
        assert_eq!(
            s.clients[ClientRole::StandaloneProbe.index()].http[index].body_eof,
            0
        );
        assert_eq!(
            s.clients[ClientRole::StandaloneProbe.index()].http[index].body_dropped,
            1
        );
        let (inner, polls, _) = body(Vec::new(), false, true);
        let fixture = Fixture::new(vec![Ok(reply(200, inner))], false);
        let client = observed(&fixture, &observer, ClientRole::StandaloneProbe);
        let mut response = client
            .execute(request(method, Bytes::new()))
            .await
            .unwrap()
            .into_body();
        assert!(matches!(poll_body(&mut response), Poll::Ready(None)));
        drop(response);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        let s = observer.snapshot().unwrap();
        assert_eq!(
            s.clients[ClientRole::StandaloneProbe.index()].http[index].body_eof,
            1
        );
        assert_eq!(
            s.clients[ClientRole::StandaloneProbe.index()].http[index].body_dropped,
            1
        );
    }
}

#[test]
fn configured_four_clients_roles_budgets_sharing_and_final_bundle_release_are_preserved() {
    let observer = Observer::isolated();
    let fixture = Fixture::new(Vec::new(), false);
    let store = RustFsBlockStore::from_config_with_builder(
        "private-unit",
        true,
        observer.clone(),
        |probe, role, observer| {
            Ok(Arc::new(config().build_client_with_injected_connector(
                probe,
                role,
                observer,
                connector(&fixture),
            )?) as Arc<dyn ObjectStore>)
        },
    )
    .unwrap();
    assert_eq!(
        *fixture.options.lock().unwrap(),
        vec![
            (Some("30s".into()), Some("5s".into())),
            (Some("30s".into()), Some("5s".into())),
            (Some("8s".into()), Some("3s".into())),
            (Some("8s".into()), Some("3s".into())),
        ]
    );
    let clients = store.qualification_clients.as_ref().unwrap();
    assert!(Arc::ptr_eq(
        store.configured_probe.as_ref().unwrap(),
        &clients.first_probe
    ));
    let weak = [
        Arc::downgrade(&clients.first_data),
        Arc::downgrade(&clients.second_data),
        Arc::downgrade(&clients.first_probe),
        Arc::downgrade(&clients.second_probe),
    ];
    assert!(!Arc::ptr_eq(&clients.first_data, &clients.second_data));
    assert!(!Arc::ptr_eq(&clients.first_probe, &clients.second_probe));
    let handle = store.http_observer();
    let clone = store.clone();
    drop(store);
    assert!(weak.iter().all(|owner| owner.upgrade().is_some()));
    assert_eq!(handle.snapshot().unwrap().bundles.live, 1);
    drop(clone);
    assert!(weak.iter().all(|owner| owner.upgrade().is_none()));
    let s = handle.snapshot().unwrap();
    assert_eq!(s.bundles.committed, 1);
    assert_eq!(s.bundles.live, 0);
    assert_eq!(s.bundles.released, 1);
    for role in [
        ClientRole::PrimaryDataMixed,
        ClientRole::QualificationData,
        ClientRole::PrimaryProbeMixed,
        ClientRole::QualificationProbe,
    ] {
        assert_eq!(s.clients[role.index()].constructed, 1);
        assert_eq!(s.clients[role.index()].released, 1);
        assert_eq!(s.clients[role.index()].live, 0);
    }
}

#[test]
fn actual_fourth_client_build_failure_releases_prior_clients_and_never_commits_bundle() {
    let observer = Observer::isolated();
    let fixture = Fixture::new(Vec::new(), false);
    let result = RustFsBlockStore::from_config_with_builder(
        "private-unit",
        true,
        observer.clone(),
        |probe, role, observer| {
            let failed = matches!(role, ClientRole::QualificationProbe);
            Ok(Arc::new(config().build_client_with_injected_connector(
                probe,
                role,
                observer,
                FixtureConnector {
                    fixture: fixture.clone(),
                    fail: failed,
                },
            )?) as Arc<dyn ObjectStore>)
        },
    );
    assert!(result.is_err());
    let s = observer.snapshot().unwrap();
    assert_eq!(fixture.options.lock().unwrap().len(), 4);
    assert_eq!(s.bundles.committed, 0);
    assert_eq!(s.bundles.failed, 1);
    assert_eq!(s.bundles.live, 0);
    assert_eq!(s.bundles.abandoned, 0);
    for role in [
        ClientRole::PrimaryDataMixed,
        ClientRole::QualificationData,
        ClientRole::PrimaryProbeMixed,
    ] {
        assert_eq!(s.clients[role.index()].constructed, 1);
        assert_eq!(s.clients[role.index()].released, 1);
        assert_eq!(s.clients[role.index()].live, 0);
    }
    assert_eq!(
        s.clients[ClientRole::QualificationProbe.index()].constructed,
        0
    );
}

#[test]
fn invalid_prefix_after_four_actual_client_builds_records_bundle_error_and_releases_all() {
    let observer = Observer::isolated();
    let fixture = Fixture::new(Vec::new(), false);
    let result = RustFsBlockStore::from_config_with_builder(
        "",
        true,
        observer.clone(),
        |probe, role, observer| {
            Ok(Arc::new(config().build_client_with_injected_connector(
                probe,
                role,
                observer,
                connector(&fixture),
            )?) as Arc<dyn ObjectStore>)
        },
    );
    assert!(result.is_err());
    let s = observer.snapshot().unwrap();
    assert_eq!(fixture.options.lock().unwrap().len(), 4);
    assert_eq!(s.bundles.failed, 1);
    assert_eq!(s.bundles.committed, 0);
    for role in [
        ClientRole::PrimaryDataMixed,
        ClientRole::QualificationData,
        ClientRole::PrimaryProbeMixed,
        ClientRole::QualificationProbe,
    ] {
        assert_eq!(s.clients[role.index()].constructed, 1);
        assert_eq!(s.clients[role.index()].released, 1);
    }
}

#[test]
fn disabled_connector_returns_original_service_without_wrapper_or_observation() {
    let observer = Observer::disabled();
    let fixture = Fixture::new(Vec::new(), true);
    let client = observed(&fixture, &observer, ClientRole::StandaloneData);
    {
        let future = client.execute(request("GET", Bytes::new()));
        let mut future = std::pin::pin!(future);
        assert!(poll_once(future.as_mut()).is_pending());
    }
    assert!(observer.snapshot().is_none());
    assert_eq!(fixture.entered.load(Ordering::SeqCst), 1);
}

// Actual allocation controls are intentionally limited to this wrapper seam.
// The fixture does no network/runtime I/O and all requests/responses/banks are
// prebuilt. Counters describe allocator calls observed on this test thread.
mod allocation_control {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    thread_local! {
        static TRACK: Cell<bool> = const { Cell::new(false) };
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }
    struct Counting;
    #[global_allocator]
    static ALLOCATOR: Counting = Counting;
    fn count() {
        let _ = TRACK.try_with(|track| {
            if track.get() {
                let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
            }
        });
    }
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count();
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            count();
            unsafe { System.realloc(ptr, layout, size) }
        }
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TRACK.with(|track| track.set(false));
        }
    }
    fn measured(action: impl FnOnce()) -> usize {
        TRACK.with(|track| assert!(!track.get()));
        ALLOCS.with(|n| n.set(0));
        TRACK.with(|track| track.set(true));
        let reset = Reset;
        action();
        drop(reset);
        ALLOCS.with(Cell::get)
    }
    fn positive() {
        assert!(
            measured(|| {
                let mut bytes = Vec::<u8>::with_capacity(4096);
                bytes.extend_from_slice(b"allocation-positive");
                std::hint::black_box(bytes);
            }) >= 1
        );
    }
    #[test]
    fn pending_call_adds_exactly_one_box_and_disabled_call_preserves_baseline() {
        positive();
        let observer = Observer::isolated();
        let plain = Fixture::new(Vec::new(), true);
        let enabled = Fixture::new(Vec::new(), true);
        let disabled = Fixture::new(Vec::new(), true);
        let direct = FixtureService(plain.clone());
        let wrapped = ObservedService::new(
            HttpClient::new(FixtureService(enabled.clone())),
            observer.clone(),
            ClientRole::StandaloneData,
        );
        let unwrapped = observed(&disabled, &Observer::disabled(), ClientRole::StandaloneData);
        let a = request("GET", Bytes::new());
        let b = request("GET", Bytes::new());
        let c = request("GET", Bytes::new());
        // Warm TLS/metrics/clock before the measured windows; no fixture call yet.
        std::hint::black_box(observer.snapshot());
        std::hint::black_box(std::time::Instant::now());
        let baseline = measured(|| {
            let mut f = direct.call(a);
            assert!(poll_once(f.as_mut()).is_pending());
            drop(f);
        });
        let observed_count = measured(|| {
            let mut f = wrapped.call(b);
            assert!(poll_once(f.as_mut()).is_pending());
            drop(f);
        });
        let disabled_count = measured(|| {
            let f = unwrapped.execute(c);
            let mut f = std::pin::pin!(f);
            assert!(poll_once(f.as_mut()).is_pending());
        });
        assert_eq!(observed_count, baseline + 1);
        assert_eq!(disabled_count, baseline);
        assert_eq!(plain.exited.load(Ordering::SeqCst), 1);
        assert_eq!(enabled.exited.load(Ordering::SeqCst), 1);
        assert_eq!(disabled.exited.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn ready_prebuilt_response_adds_one_future_box_and_one_body_box() {
        positive();
        let observer = Observer::isolated();
        let plain = Fixture::new(vec![Ok(reply(200, Bytes::new().into()))], false);
        let enabled = Fixture::new(vec![Ok(reply(200, Bytes::new().into()))], false);
        let direct = FixtureService(plain);
        let wrapped = ObservedService::new(
            HttpClient::new(FixtureService(enabled)),
            observer.clone(),
            ClientRole::StandaloneData,
        );
        let a = request("GET", Bytes::new());
        let b = request("GET", Bytes::new());
        std::hint::black_box(observer.snapshot());
        std::hint::black_box(std::time::Instant::now());
        let baseline = measured(|| {
            let mut f = direct.call(a);
            assert!(matches!(poll_once(f.as_mut()), Poll::Ready(Ok(_))));
        });
        let observed_count = measured(|| {
            let mut f = wrapped.call(b);
            assert!(matches!(poll_once(f.as_mut()), Poll::Ready(Ok(_))));
        });
        assert_eq!(observed_count, baseline + 2);
        let s = observer.snapshot().unwrap();
        let row = &s.clients[ClientRole::StandaloneData.index()].http[HttpMethod::Get.index()];
        assert_eq!(row.known_extra_future_boxes, 1);
        assert_eq!(row.known_extra_response_body_boxes, 1);
    }
}
