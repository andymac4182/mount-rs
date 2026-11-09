//! Diagnostic buckets only; never used for admission, authorization or retry.
#[derive(Clone, Copy)]
pub(crate) enum RequestClass {
    Read,
    Mutation,
    SessionRpc,
}
impl RequestClass {
    pub(crate) fn classify<T>(request: &http::Request<T>) -> Self {
        let session = request
            .uri()
            .path()
            .strip_prefix("/v1/drives/")
            .and_then(|path| path.split_once('/'))
            .is_some_and(|(volume, tail)| {
                !volume.is_empty() && matches!(tail, "sessions" | "sessions/handles")
            });
        if session {
            Self::SessionRpc
        } else if matches!(*request.method(), http::Method::GET | http::Method::HEAD) {
            Self::Read
        } else {
            Self::Mutation
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_routes_are_separate_from_file_methods() {
        for method in ["POST", "PUT", "DELETE", "GET"] {
            for path in ["/v1/drives/v/sessions", "/v1/drives/v/sessions/handles"] {
                let request = http::Request::builder()
                    .method(method)
                    .uri(path)
                    .body(())
                    .unwrap();
                assert!(matches!(
                    RequestClass::classify(&request),
                    RequestClass::SessionRpc
                ));
            }
        }
        for path in [
            "/v1/drives/v/fs/sessions",
            "/v1/drives/v/sessions-other",
            "/v1/drives//sessions",
            "/v1/drives/v/sessions/handles/extra",
        ] {
            let request = http::Request::builder()
                .method("POST")
                .uri(path)
                .body(())
                .unwrap();
            assert!(matches!(
                RequestClass::classify(&request),
                RequestClass::Mutation
            ));
        }
        for method in ["GET", "HEAD"] {
            let request = http::Request::builder()
                .method(method)
                .uri("/v1/drives/v/fs/file")
                .body(())
                .unwrap();
            assert!(matches!(
                RequestClass::classify(&request),
                RequestClass::Read
            ));
        }
    }
}
