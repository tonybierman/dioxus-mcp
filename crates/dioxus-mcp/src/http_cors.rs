//! Minimal CORS wrapper for the Streamable HTTP transport.
//!
//! A browser-based client (e.g. the `dx-playground` DSL editor) talks to this
//! server cross-origin, so it needs CORS preflight handling plus an *exposed*
//! `Mcp-Session-Id` response header — without `Access-Control-Expose-Headers`,
//! browser `fetch` cannot read the session id and the MCP handshake silently
//! fails.
//!
//! We wrap the rmcp [`StreamableHttpService`](rmcp::transport::streamable_http_server::StreamableHttpService)
//! by hand rather than pulling in `tower-http`: its `CorsLayer` requires
//! `ResBody: Default` (to synthesize an empty preflight body), and the rmcp
//! service's `BoxBody<Bytes, Infallible>` response body does not implement
//! `Default`. Wrapping by hand keeps real (streaming SSE) responses untouched
//! and only adds headers / short-circuits `OPTIONS`.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Empty, combinators::BoxBody};
use tower::Service;

/// The rmcp streamable-HTTP service's response type.
type BoxResponse = Response<BoxBody<Bytes, Infallible>>;

/// Adds permissive CORS headers to every response and answers `OPTIONS`
/// preflight requests with `204 No Content`. Generic over the wrapped service
/// so it composes directly inside `TowerToHyperService`.
#[derive(Clone)]
pub struct Cors<S> {
    inner: S,
}

impl<S> Cors<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

fn apply_cors_headers(headers: &mut http::HeaderMap) {
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type, mcp-session-id, mcp-protocol-version"),
    );
    // Critical: browser fetch can only read the session id off the response
    // when it is explicitly exposed.
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("mcp-session-id"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
}

impl<S, B> Service<Request<B>> for Cors<S>
where
    S: Service<Request<B>, Response = BoxResponse, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = BoxResponse;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<BoxResponse, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        if req.method() == Method::OPTIONS {
            return Box::pin(async move {
                let mut res = Response::new(Empty::<Bytes>::new().boxed());
                *res.status_mut() = StatusCode::NO_CONTENT;
                apply_cors_headers(res.headers_mut());
                Ok(res)
            });
        }
        // Clone-and-swap so the cloned (ready) service is the one we call — the
        // standard tower idiom for `&mut self` services used across tasks.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let mut res = inner.call(req).await?;
            apply_cors_headers(res.headers_mut());
            Ok(res)
        })
    }
}
