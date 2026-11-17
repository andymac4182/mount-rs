//! How the contract suites start the server they test.
//!
//! Each suite is built twice: once against the listener's own routes, and
//! once behind a request handler that passes every request on to an
//! application without a listener. The second is the path a request takes
//! in a server that has `remote_sessions` or `forwarding` configured: there
//! the listener hands each request to a router, and the router hands a
//! request for a drive of its own to exactly such an application. Whatever
//! the suites assert must hold on both paths.

use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;

use mount_rs_http::{
    DriveRegistry, HttpBody, HttpRequestHandler, HttpServer, HttpServerError, HttpServerOptions,
};

/// The server under test: its listener and, behind a handler, the
/// application the handler passes requests to.
pub struct Served {
    listener: HttpServer,
    application: Option<Arc<HttpServer>>,
}

impl Deref for Served {
    type Target = HttpServer;
    fn deref(&self) -> &HttpServer {
        &self.listener
    }
}

impl Served {
    /// Close the listener and then the application, in the order
    /// `serve-http` closes them. The first failure is the result.
    pub async fn close(&self) -> Result<(), HttpServerError> {
        let listener = self.listener.close().await;
        let application = match &self.application {
            Some(application) => application.close().await,
            None => Ok(()),
        };
        listener.and(application)
    }
}

struct PassOn(Arc<HttpServer>);

impl HttpRequestHandler for PassOn {
    fn handle(
        &self,
        request: hyper::Request<HttpBody>,
    ) -> Pin<Box<dyn std::future::Future<Output = hyper::Response<HttpBody>> + Send + '_>> {
        Box::pin(self.0.handle(request))
    }
}

/// Start a server for `registry`, behind a handler when `behind_handler`.
/// Both the listener and the application get `options`, as in `serve-http`.
pub async fn serve(
    registry: DriveRegistry,
    options: HttpServerOptions,
    behind_handler: bool,
) -> Served {
    if !behind_handler {
        return Served {
            listener: HttpServer::start(registry, options)
                .await
                .expect("HTTP server"),
            application: None,
        };
    }
    let application = Arc::new(HttpServer::unbound(registry, options.clone()));
    let listener = HttpServer::start_with_handler(Arc::new(PassOn(application.clone())), options)
        .await
        .expect("HTTP server behind a handler");
    Served {
        listener,
        application: Some(application),
    }
}
