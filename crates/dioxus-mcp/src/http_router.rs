//! Serves the embedded dx-playground UI ("cockpit") at the same origin as the
//! MCP endpoint, so one `dioxus-mcp --transport http` instance hosts both the
//! protocol and the human UI (no CORS needed for the bundled case).
//!
//! [`UiRouter`] is a hand-rolled tower [`Service`] layered *outside*
//! [`Cors`](crate::http_cors::Cors)`<StreamableHttpService>`. rmcp's service
//! dispatches purely by HTTP method and ignores the path, so we can claim a few
//! GET paths (`/`, `/index.html`, `/assets/*`, `/favicon.ico`) for static UI and
//! delegate everything else — including the MCP POSTs — to the inner service.
//! This relies on the browser MCP client only ever POSTing; if a future client
//! opens a bare `GET /` SSE stream, mount MCP under an explicit prefix instead.
//!
//! Assets are embedded at compile time via `include_dir!` from `ui-dist/`, with
//! a `DIOXUS_MCP_UI_DIR` env override that reads a live directory per request
//! (for UI iteration without recompiling the server).

use std::convert::Infallible;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use tower::Service;

type BoxResponse = Response<BoxBody<Bytes, Infallible>>;

/// The committed placeholder `index.html` carries this marker so the server can
/// warn at startup that the real UI bundle hasn't been built in yet.
const PLACEHOLDER_MARK: &str = "DIOXUS_MCP_UI_PLACEHOLDER";

static EMBEDDED_UI: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/ui-dist");

/// Where UI bytes come from: the compiled-in bundle, or a live directory.
pub enum UiAssets {
    Embedded,
    Dev { root: PathBuf },
}

impl UiAssets {
    /// `DIOXUS_MCP_UI_DIR=<path>` serves that directory live; otherwise the
    /// embedded bundle is used.
    pub fn from_env() -> Arc<Self> {
        match std::env::var("DIOXUS_MCP_UI_DIR") {
            Ok(dir) if !dir.trim().is_empty() => Arc::new(UiAssets::Dev {
                root: PathBuf::from(dir),
            }),
            _ => Arc::new(UiAssets::Embedded),
        }
    }

    /// Map a request path to the asset we'd serve, or `None` to delegate to MCP.
    fn rel_for(path: &str) -> Option<&str> {
        match path {
            "/" | "/index.html" => Some("index.html"),
            "/favicon.ico" => Some("favicon.ico"),
            p if p.starts_with("/assets/") => Some(p.trim_start_matches('/')),
            _ => None,
        }
    }

    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        match self {
            UiAssets::Embedded => EMBEDDED_UI.get_file(rel).map(|f| f.contents().to_vec()),
            UiAssets::Dev { root } => {
                // Reject path traversal before touching the filesystem.
                if rel.split('/').any(|c| c == ".." || c.is_empty()) {
                    return None;
                }
                std::fs::read(root.join(rel)).ok()
            }
        }
    }

    /// True when a real (non-placeholder) UI bundle is present.
    pub fn ui_built(&self) -> bool {
        match self.read("index.html") {
            Some(bytes) => !String::from_utf8_lossy(&bytes).contains(PLACEHOLDER_MARK),
            None => false,
        }
    }

    /// Serve `path` if it's a UI path. Returns `Some(404)` for a missing
    /// `/assets/*` (a real asset miss shouldn't fall through to MCP), and `None`
    /// for non-UI paths so the caller delegates to the MCP service.
    pub fn try_serve(&self, path: &str) -> Option<BoxResponse> {
        let rel = Self::rel_for(path)?;
        Some(match self.read(rel) {
            Some(bytes) => ok_bytes(bytes, content_type(rel)),
            None => not_found(),
        })
    }
}

fn content_type(rel: &str) -> &'static str {
    match rel.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        // Mandatory: the browser refuses to streaming-compile wasm with the
        // wrong content-type.
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

fn ok_bytes(bytes: Vec<u8>, content_type: &'static str) -> BoxResponse {
    let body = Full::new(Bytes::from(bytes)).boxed();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        // Hashed asset names make caching safe, but no-cache avoids stale
        // bundles during dev (DIOXUS_MCP_UI_DIR).
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .body(body)
        .expect("valid response")
}

fn not_found() -> BoxResponse {
    let body = Full::new(Bytes::from_static(b"404 Not Found")).boxed();
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(body)
        .expect("valid response")
}

/// Routes static UI GETs to [`UiAssets`]; delegates everything else to `inner`.
#[derive(Clone)]
pub struct UiRouter<S> {
    inner: S,
    assets: Arc<UiAssets>,
}

impl<S> UiRouter<S> {
    pub fn new(inner: S, assets: Arc<UiAssets>) -> Self {
        Self { inner, assets }
    }
}

impl<S, B> Service<Request<B>> for UiRouter<S>
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
        if req.method() == Method::GET
            && let Some(resp) = self.assets.try_serve(req.uri().path())
        {
            return Box::pin(async move { Ok(resp) });
        }
        // Clone-and-swap so the cloned (ready) service is the one called — the
        // standard tower idiom, mirroring http_cors.rs.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move { inner.call(req).await })
    }
}
