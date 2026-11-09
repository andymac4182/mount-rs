//! Authenticated session lifecycle routes. Owner fencing remains enforced by
//! the SDK-backed registry. The caller owns TLS, expiry and shutdown draining.
use crate::{SessionCredentials, SessionToken, VolumeSessions};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_http::{HttpBody, HttpRequestHandler};
use std::{pin::Pin, sync::Arc, time::Duration};

#[path = "session_http_ops.rs"]
mod operations;

pub const SESSION_HEADER: &str = "x-mount-session";
/// One volume's lifecycle endpoint. Creation returns a random session token;
/// close takes it in a header so it need not appear in URLs/access logs.
/// A lost creation response leaves a bounded session for idle expiry. Clients
/// must not automatically replay creation or other state-changing requests.
pub struct SessionHttpHandler {
    path: String,
    handles_path: String,
    credentials: SessionCredentials,
    registry: Arc<VolumeSessions>,
    max_request_bytes: usize,
}
impl SessionHttpHandler {
    pub fn registry(&self) -> Arc<VolumeSessions> {
        self.registry.clone()
    }
    pub fn new(
        volume: &str,
        credentials: SessionCredentials,
        registry: Arc<VolumeSessions>,
    ) -> Result<Self> {
        if matches!(volume, "" | "." | "..")
            || volume.len() > 128
            || !volume
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(FsError::new(ErrorCode::Einval).with_syscall("session"));
        }
        Ok(Self {
            path: format!("/v1/drives/{volume}/sessions"),
            handles_path: format!("/v1/drives/{volume}/sessions/handles"),
            credentials,
            registry,
            max_request_bytes: 65536,
        })
    }
    /// Apply a server body limit without raising the protocol's own limits.
    pub fn with_max_request_bytes(mut self, limit: usize) -> Result<Self> {
        if limit == 0 {
            return Err(FsError::new(ErrorCode::Einval).with_syscall("session"));
        }
        self.max_request_bytes = limit;
        Ok(self)
    }
    pub fn matches(&self, path: &str) -> bool {
        path == self.path || path == self.handles_path
    }
    pub(crate) async fn serve(
        &self,
        request: Request<HttpBody>,
        timings: Option<&crate::owner_http::SessionOperationCounters>,
    ) -> Response<HttpBody> {
        if !self.matches(request.uri().path()) {
            return rejected(StatusCode::NOT_FOUND, ErrorCode::Enoent);
        }
        let principal = match self.credentials.authenticate(request.headers()) {
            Ok(principal) => principal,
            Err(_) => return rejected(StatusCode::UNAUTHORIZED, ErrorCode::Eacces),
        };
        if request.uri().query().is_some() {
            return rejected(StatusCode::BAD_REQUEST, ErrorCode::Einval);
        }
        if request.uri().path() == self.handles_path {
            return operations::serve(
                request,
                &self.registry,
                principal,
                self.max_request_bytes,
                timings,
            )
            .await;
        }
        let method = request.method().clone();
        if method != Method::POST && method != Method::DELETE {
            let mut response = rejected(StatusCode::METHOD_NOT_ALLOWED, ErrorCode::Enosys);
            response
                .headers_mut()
                .insert(http::header::ALLOW, "POST, DELETE".parse().unwrap());
            return response;
        }
        let values: Vec<_> = request.headers().get_all(SESSION_HEADER).iter().collect();
        let token = if method == Method::DELETE {
            if values.len() != 1 {
                return rejected(StatusCode::BAD_REQUEST, ErrorCode::Einval);
            }
            match values[0]
                .to_str()
                .ok()
                .and_then(|value| SessionToken::from_wire(value).ok())
            {
                Some(token) => Some(token),
                None => return rejected(StatusCode::BAD_REQUEST, ErrorCode::Einval),
            }
        } else {
            if !values.is_empty() {
                return rejected(StatusCode::BAD_REQUEST, ErrorCode::Einval);
            }
            None
        };
        // Lifecycle operations accept no payload. Read EOF before dispatch so
        // malformed/failed uploads cannot be acknowledged as a successful op.
        match tokio::time::timeout(Duration::from_secs(5), empty_body(request.into_body())).await {
            Ok(true) => {}
            Ok(false) => return rejected(StatusCode::BAD_REQUEST, ErrorCode::Einval),
            Err(_) => return rejected(StatusCode::REQUEST_TIMEOUT, ErrorCode::Eintr),
        }
        if let Some(token) = token {
            match self.registry.close_session(principal, &token).await {
                Ok(()) => response(StatusCode::NO_CONTENT, Bytes::new()),
                Err(error) => filesystem_error(error),
            }
        } else {
            match self.registry.create(principal).await {
                Ok(token) => response(
                    StatusCode::CREATED,
                    Bytes::from(
                        serde_json::json!({
                            "protocol_version":1,"session":token.to_wire(),
                            "idle_timeout_ms": self.registry.idle_timeout().as_millis()
                        })
                        .to_string(),
                    ),
                ),
                Err(error) => filesystem_error(error),
            }
        }
    }
}
impl HttpRequestHandler for SessionHttpHandler {
    fn handle(
        &self,
        request: Request<HttpBody>,
    ) -> Pin<Box<dyn std::future::Future<Output = Response<HttpBody>> + Send + '_>> {
        Box::pin(self.serve(request, None))
    }
}
async fn empty_body(mut body: HttpBody) -> bool {
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(frame) => {
                if frame.data_ref().is_some_and(|data| !data.is_empty()) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

fn response(status: StatusCode, bytes: Bytes) -> Response<HttpBody> {
    Response::builder()
        .status(status)
        .header(http::header::CACHE_CONTROL, "no-store")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(
            Full::new(bytes)
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap()
}
fn rejected(status: StatusCode, code: ErrorCode) -> Response<HttpBody> {
    let mut response = response(
        status,
        Bytes::from(
            serde_json::json!({"error":{"code":code.as_str(),"syscall":"session"}}).to_string(),
        ),
    );
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(http::header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    }
    response
}
fn filesystem_error(error: FsError) -> Response<HttpBody> {
    let status = match error.code {
        ErrorCode::Eacces => StatusCode::FORBIDDEN,
        ErrorCode::Enfile | ErrorCode::Emfile => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::Ebadf | ErrorCode::Estale => StatusCode::CONFLICT,
        ErrorCode::Eagain => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Einval => StatusCode::BAD_REQUEST,
        ErrorCode::Efbig => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::Eintr => StatusCode::REQUEST_TIMEOUT,
        ErrorCode::Enoent => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    rejected(status, error.code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn configured_body_limit_rejects_fixed_and_chunked_uploads_before_mutation() {
        let fs = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                "session-body-limit",
                4096,
            ))
            .await
            .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(fs.clone(), 1, 2, Duration::from_secs(60)).unwrap());
        let token = registry.create("alice").await.unwrap();
        let session = registry.lookup("alice", &token).unwrap();
        let handle = session.open("/limited", "w+", 0o600).await.unwrap();
        let secret = "a".repeat(32);
        let handler = SessionHttpHandler::new(
            "volume",
            SessionCredentials::new(vec![("alice".into(), secret.clone())]).unwrap(),
            registry.clone(),
        )
        .unwrap()
        .with_max_request_bytes(128)
        .unwrap();
        let server =
            mount_rs_http::HttpServer::start_with_handler(Arc::new(handler), Default::default())
                .await
                .unwrap();
        let client = reqwest::Client::new();
        let url = format!("{}/v1/drives/volume/sessions/handles", server.url());
        for body in [
            reqwest::Body::from(vec![1; 129]),
            reqwest::Body::wrap_stream(futures_util::stream::iter([
                Ok::<_, std::io::Error>(Bytes::from(vec![2; 80])),
                Ok::<_, std::io::Error>(Bytes::from(vec![3; 80])),
            ])),
        ] {
            let response = client
                .put(&url)
                .bearer_auth(&secret)
                .header(SESSION_HEADER, token.to_wire())
                .header("x-mount-handle", handle)
                .header("x-mount-position", 0)
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(session.stat(handle).await.unwrap().size, 0);
        }
        let response = client
            .post(&url)
            .bearer_auth(&secret)
            .header(SESSION_HEADER, token.to_wire())
            .body(" ".repeat(129))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let capabilities = client
            .post(&url)
            .bearer_auth(&secret)
            .header(SESSION_HEADER, token.to_wire())
            .body(r#"{"operation":"capabilities"}"#)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let capabilities: serde_json::Value = serde_json::from_slice(&capabilities).unwrap();
        assert_eq!(capabilities["max_io_bytes"], 128);
        server.close().await.unwrap();
        registry.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn real_http_auth_create_close_and_payload_rejection() {
        let fs = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                "session-http",
                4096,
            ))
            .await
            .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(fs.clone(), 1, 2, Duration::from_secs(60)).unwrap());
        let alice = "a".repeat(32);
        let bob = "b".repeat(32);
        let handler = Arc::new(
            SessionHttpHandler::new(
                "volume",
                SessionCredentials::new(vec![
                    ("alice".into(), alice.clone()),
                    ("bob".into(), bob.clone()),
                ])
                .unwrap(),
                registry.clone(),
            )
            .unwrap(),
        );
        let server = mount_rs_http::HttpServer::start_with_handler(
            handler,
            mount_rs_http::HttpServerOptions::default(),
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let url = format!("{}/v1/drives/volume/sessions", server.url());
        assert_eq!(
            client.post(&url).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&alice)
                .body("unexpected")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let created = client
            .post(&url)
            .bearer_auth(&alice)
            .header("x-mount-client", "bob")
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[http::header::CACHE_CONTROL], "no-store");
        let payload: serde_json::Value =
            serde_json::from_str(&created.text().await.unwrap()).unwrap();
        let token = payload["session"].as_str().unwrap();
        assert_eq!(payload["protocol_version"], 1);
        let session = registry
            .lookup("alice", &SessionToken::from_wire(token).unwrap())
            .unwrap();
        let handles_url = format!("{url}/handles");
        for operation in [
            serde_json::json!({"operation":"capabilities"}),
            serde_json::json!({"operation":"path","request":{"action":"stat","path":"/"}}),
            serde_json::json!({"operation":"path","request":{"action":"readdir","path":"/"}}),
            serde_json::json!({"operation":"path","request":{"action":"statfs","path":"/"}}),
        ] {
            let result = client
                .post(&handles_url)
                .bearer_auth(&alice)
                .header(SESSION_HEADER, token)
                .body(operation.to_string())
                .send()
                .await
                .unwrap();
            assert_eq!(result.status(), StatusCode::OK);
            let _: serde_json::Value = serde_json::from_str(&result.text().await.unwrap()).unwrap();
        }

        let open = client.post(&handles_url).bearer_auth(&alice).header(SESSION_HEADER, token)
            .body(serde_json::json!({"operation":"open","path":"/retained","mode":384,
                "flags":{"read":true,"write":true,"create":true,"truncate":true,"append":false,"exclusive":false}}).to_string())
            .send().await.unwrap();
        assert_eq!(open.status(), StatusCode::OK);
        let opened: serde_json::Value = serde_json::from_str(&open.text().await.unwrap()).unwrap();
        let id = opened["handle"].as_u64().unwrap();
        let write = client
            .put(&handles_url)
            .bearer_auth(&alice)
            .header(SESSION_HEADER, token)
            .header("x-mount-handle", id)
            .header("x-mount-position", 0)
            .body("retained")
            .send()
            .await
            .unwrap();
        assert_eq!(write.status(), StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&write.text().await.unwrap()).unwrap()["written"],
            8
        );
        for request in [
            serde_json::json!({"action":"rename","source":"/retained","destination":"/renamed"}),
            serde_json::json!({"action":"unlink","path":"/renamed"}),
        ] {
            let result = client
                .post(&handles_url)
                .bearer_auth(&alice)
                .header(SESSION_HEADER, token)
                .body(serde_json::json!({"operation":"path","request":request}).to_string())
                .send()
                .await
                .unwrap();
            assert_eq!(result.status(), StatusCode::NO_CONTENT);
        }
        let replacement = fs.driver().open("/retained", "w", 0o600).await.unwrap();
        replacement.write(b"new", Some(0)).await.unwrap();
        replacement.close().await.unwrap();
        let read = client
            .post(&handles_url)
            .bearer_auth(&alice)
            .header(SESSION_HEADER, token)
            .body(
                serde_json::json!({"operation":"read","handle":id,"length":8,"position":0})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(read.status(), StatusCode::OK);
        assert_eq!(
            read.headers()[http::header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(read.bytes().await.unwrap().as_ref(), b"retained");
        let oversized = client
            .put(&handles_url)
            .bearer_auth(&alice)
            .header(SESSION_HEADER, token)
            .header("x-mount-handle", id)
            .body(vec![b'x'; 65537])
            .send()
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
        oversized.bytes().await.unwrap();
        assert_eq!(session.stat(id).await.unwrap().size, 8);
        for operation in [
            serde_json::json!({"operation":"sync","handle":id,"data_only":true}),
            serde_json::json!({"operation":"truncate","handle":id,"length":4}),
        ] {
            assert_eq!(
                client
                    .post(&handles_url)
                    .bearer_auth(&alice)
                    .header(SESSION_HEADER, token)
                    .body(operation.to_string())
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NO_CONTENT
            );
        }
        assert_eq!(session.stat(id).await.unwrap().size, 4);
        let stat = client
            .post(&handles_url)
            .bearer_auth(&alice)
            .header(SESSION_HEADER, token)
            .body(serde_json::json!({"operation":"stat","handle":id}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(stat.status(), StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stat.text().await.unwrap()).unwrap()["size"],
            4
        );
        for operation in [
            serde_json::json!({"operation":"read","handle":id,"length":65537,"position":0}),
            serde_json::json!({"operation":"stat","handle":id,"unexpected":true}),
        ] {
            let failed = client
                .post(&handles_url)
                .bearer_auth(&alice)
                .header(SESSION_HEADER, token)
                .body(operation.to_string())
                .send()
                .await
                .unwrap();
            assert!(failed.status().is_client_error());
            failed.bytes().await.unwrap();
        }
        let closed = client
            .post(&handles_url)
            .bearer_auth(&alice)
            .header(SESSION_HEADER, token)
            .body(serde_json::json!({"operation":"close","handle":id}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(closed.status(), StatusCode::NO_CONTENT);
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        let id = session.open("/retained", "r", 0).await.unwrap();

        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&alice)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            client
                .delete(&url)
                .bearer_auth(&bob)
                .header(SESSION_HEADER, token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert!(session.stat(id).await.is_ok());
        assert_eq!(
            client
                .delete(&url)
                .bearer_auth(&alice)
                .header(SESSION_HEADER, token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        let next = client.post(&url).bearer_auth(&bob).send().await.unwrap();
        assert_eq!(next.status(), StatusCode::CREATED);
        // Listener drain and registry drain are separate explicit lifecycle steps.
        server.close().await.unwrap();
        registry.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }
}
