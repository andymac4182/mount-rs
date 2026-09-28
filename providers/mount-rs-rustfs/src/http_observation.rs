//! Opt-in observations at the public object_store HTTP service boundary.
//! Counts object_store dispatches (including its retries), not exact wire requests:
//! reqwest redirects/protocol retries and connection recovery remain below this seam.
//! Metrics contain closed labels and counters only; no URI, headers, credentials or data.
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use mount_rs_core::diagnostics::object_store::{
    ClientRole, HttpBodyGuard, HttpClientGuard, HttpMethod, Observer,
};
use object_store::ClientOptions;
use object_store::client::{
    HttpClient, HttpConnector, HttpError, HttpRequest, HttpResponse, HttpResponseBody, HttpService,
};

pub(super) struct ObservedConnector<C> {
    inner: C,
    observer: Observer,
    role: ClientRole,
}
impl<C> ObservedConnector<C> {
    pub(super) fn new(inner: C, observer: Observer, role: ClientRole) -> Self {
        Self {
            inner,
            observer,
            role,
        }
    }
}
impl<C> fmt::Debug for ObservedConnector<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ObservedConnector")
    }
}
impl<C: HttpConnector> HttpConnector for ObservedConnector<C> {
    fn connect(&self, options: &ClientOptions) -> object_store::Result<HttpClient> {
        if !self.observer.is_enabled() {
            return self.inner.connect(options);
        }
        // Time only the actual inner construction, preserving options and typed errors.
        let build = self.observer.client_build(self.role);
        let inner = match self.inner.connect(options) {
            Ok(inner) => {
                build.finish_success();
                inner
            }
            Err(error) => {
                build.finish_error();
                return Err(error);
            }
        };
        Ok(HttpClient::new(ObservedService::new(
            inner,
            self.observer.clone(),
            self.role,
        )))
    }
}
pub(super) struct ObservedService {
    inner: HttpClient,
    observer: Observer,
    role: ClientRole,
    // Last field: release follows the inner HttpClient reference drop.
    lifetime: HttpClientGuard,
}
impl ObservedService {
    pub(super) fn new(inner: HttpClient, observer: Observer, role: ClientRole) -> Self {
        let lifetime = observer.client(role);
        Self {
            inner,
            observer,
            role,
            lifetime,
        }
    }
}
impl fmt::Debug for ObservedService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ObservedHttpService")
    }
}
fn method(request: &HttpRequest) -> HttpMethod {
    match request.method().as_str() {
        "GET" => HttpMethod::Get,
        "HEAD" => HttpMethod::Head,
        "PUT" => HttpMethod::Put,
        "DELETE" => HttpMethod::Delete,
        "POST" => HttpMethod::Post,
        _ => HttpMethod::Other,
    }
}
impl HttpService for ObservedService {
    // Match async_trait's existing HttpService signature explicitly so the known
    // extra forwarding box is counted at construction, including unpolled calls.
    // This counts one known Box::pin site, not every allocator/transport operation.
    fn call<'life0, 'async_trait>(
        &'life0 self,
        request: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        let method = method(&request);
        self.observer.known_extra_future_box(self.role, method);
        Box::pin(async move {
            // Attempt begins only when this actual future is first polled.
            // Offered bytes are not a claim about upload bytes sent or committed.
            let offered = u64::try_from(request.body().content_length()).ok();
            let attempt = self.lifetime.attempt(method, offered);
            match self.inner.execute(request).await {
                Err(error) => {
                    attempt.transport_error();
                    // Return the original typed error without formatting/reclassifying it.
                    Err(error)
                }
                Ok(response) => {
                    let body_guard = attempt.headers(response.status().as_u16());
                    let (parts, body) = response.into_parts();
                    self.observer
                        .known_extra_response_body_box(self.role, method);
                    let body = HttpResponseBody::new(ObservedBody {
                        inner: Some(body),
                        guard: Some(body_guard),
                    });
                    Ok(HttpResponse::from_parts(parts, body))
                }
            }
        })
    }
}
struct ObservedBody {
    inner: Option<HttpResponseBody>,
    guard: Option<HttpBodyGuard>,
}
impl Body for ObservedBody {
    type Data = Bytes;
    type Error = HttpError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, HttpError>>> {
        let result =
            Pin::new(self.inner.as_mut().expect("HTTP body present until Drop")).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(guard), Some(data)) = (&mut self.guard, frame.data_ref()) {
                    guard.data(data.len() as u64);
                }
            }
            Poll::Ready(Some(Err(_))) => {
                if let Some(guard) = self.guard.take() {
                    guard.error();
                }
            }
            Poll::Ready(None) => {
                if let Some(guard) = self.guard.take() {
                    guard.eof();
                }
            }
            Poll::Pending => {}
        }
        // Original frames, errors, EOF and Pending all pass through unchanged.
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner
            .as_ref()
            .expect("HTTP body present until Drop")
            .is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner
            .as_ref()
            .expect("HTTP body present until Drop")
            .size_hint()
    }
}
impl Drop for ObservedBody {
    fn drop(&mut self) {
        // Body owns only its original inner body plus metrics; it retains no
        // ObservedService/client guard. Drop the original owner before reporting
        // this local drop boundary. EOF still never proves pool/socket drain.
        drop(self.inner.take());
        // An end-stream hint is not an observed poll_frame(None). Even a
        // pre-ended/HEAD body dropped without polling is coverage: body_dropped,
        // not failure, EOF, or dispatch cancellation.
        drop(self.guard.take());
    }
}
