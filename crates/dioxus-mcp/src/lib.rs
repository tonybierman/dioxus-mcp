//! MCP server for Dioxus 0.7 projects.
//!
//! Binaries should generally just call [`run`] with a parsed [`Cli`]; the
//! `dioxus-mcp` binary in this crate does exactly that.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use tracing_subscriber::EnvFilter;

pub(crate) mod http_cors;
pub(crate) mod http_router;
pub(crate) mod project;
pub(crate) mod proposal;
pub(crate) mod registry;
pub(crate) mod server;
pub(crate) mod state;
pub(crate) mod tools;

use crate::server::DioxusMcp;
use crate::state::State;

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum Transport {
    Stdio,
    Http,
}

#[derive(clap::Parser, Debug)]
#[command(name = "dioxus-mcp", about = "MCP server for Dioxus projects")]
pub struct Cli {
    #[arg(long, value_enum, default_value_t = Transport::Stdio)]
    pub transport: Transport,

    #[arg(long, default_value = "127.0.0.1:8731")]
    pub bind: String,

    #[arg(long)]
    pub project_root: Option<PathBuf>,

    /// In stdio mode, don't also start the embedded cockpit (UI + HTTP) at
    /// `--bind`. No effect in `--transport http` mode.
    #[arg(long)]
    pub no_cockpit: bool,

    #[arg(long, default_value = "info")]
    pub log: String,
}

pub async fn run(cli: Cli) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cli.log).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let project_root = cli
        .project_root
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let state = Arc::new(State::new(project_root)?);
    tracing::info!(?state.project_root, "dioxus-mcp starting");
    // Append a one-line session record to ~/.cache/dioxus-mcp/sessions.jsonl
    // so MCP-client reconnect churn is countable post-hoc. Best-effort: any
    // I/O error is logged at debug and otherwise ignored — telemetry must
    // never block the server.
    record_session_start(&state.project_root, &cli.transport);

    match cli.transport {
        Transport::Stdio => {
            // Embed the cockpit (UI + HTTP MCP) alongside stdio unless opted out.
            // It shares this process's `Arc<State>`, so the agent's stdio
            // `propose_scaffold` and a browser's HTTP `resolve_proposal` hit one
            // proposal store. `graceful` so a busy port (another session owns it)
            // is a warning, not a crash. The task dies with the process when the
            // stdio session ends.
            if !cli.no_cockpit {
                let state = state.clone();
                let bind = cli.bind.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_http(state, bind, true).await {
                        tracing::warn!(error = %e, "embedded cockpit exited");
                    }
                });
            }
            let handler = DioxusMcp::new(state);
            let service = handler.serve(stdio()).await?;
            service.waiting().await?;
        }
        // Standalone/durable: hard-fail on a taken port (this IS the server).
        Transport::Http => serve_http(state, cli.bind.clone(), false).await?,
    }

    Ok(())
}

/// Serve the cockpit — the playground UI plus the MCP protocol — over HTTP at
/// `bind`. Used both as the standalone `--transport http` server and as the
/// embedded cockpit spawned alongside stdio. When `graceful`, a bind failure
/// (port already owned by another session's cockpit) logs a warning and returns
/// `Ok(())` instead of erroring.
async fn serve_http(state: Arc<State>, bind: String, graceful: bool) -> Result<()> {
    let ui = crate::http_router::UiAssets::from_env();
    // UiRouter (static UI) outermost → Cors → MCP. Static GETs never pay
    // CORS/MCP cost; CORS still wraps MCP for the cross-origin case.
    let mcp = crate::http_cors::Cors::new(StreamableHttpService::new(
        {
            let state = state.clone();
            move || Ok(DioxusMcp::new(state.clone()))
        },
        LocalSessionManager::default().into(),
        Default::default(),
    ));
    let svc = TowerToHyperService::new(crate::http_router::UiRouter::new(mcp, ui.clone()));

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) if graceful => {
            tracing::warn!(%bind, error = %e, "cockpit port busy — another session likely owns it; running without an embedded cockpit");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    tracing::info!(%bind, "cockpit: UI + MCP at http://{bind}/");
    if !ui.ui_built() {
        tracing::warn!(
            "UI bundle not built in — GET / serves a placeholder. \
             Run crates/dioxus-mcp/scripts/build-ui.sh and rebuild, \
             or set DIOXUS_MCP_UI_DIR."
        );
    }
    loop {
        let io = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            accept = listener.accept() => TokioIo::new(accept?.0),
        };
        let svc = svc.clone();
        tokio::spawn(async move {
            let _ = Builder::new(TokioExecutor::default())
                .serve_connection(io, svc)
                .await;
        });
    }
    Ok(())
}

/// Append a one-line JSON record describing this server start to
/// `~/.cache/dioxus-mcp/sessions.jsonl` (or `$XDG_CACHE_HOME/dioxus-mcp/...`
/// when set). MCP stdio clients spawn a fresh server process on every
/// reconnect, so a line-per-start log makes "is this client cycling us"
/// measurable with `wc -l` without baking in a heavier telemetry pipeline.
///
/// Best-effort: any failure is logged at debug and swallowed so the server
/// keeps booting cleanly on read-only home dirs, missing $HOME, etc.
fn record_session_start(project_root: &std::path::Path, transport: &Transport) {
    use std::fs::OpenOptions;
    use std::io::Write;
    let Some(cache_dir) = session_log_dir() else {
        tracing::debug!("session log: no cache dir resolvable; skipping");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        tracing::debug!(error=%e, dir=%cache_dir.display(), "session log: mkdir failed");
        return;
    }
    let log_path = cache_dir.join("sessions.jsonl");
    let transport_str = match transport {
        Transport::Stdio => "stdio",
        Transport::Http => "http",
    };
    // serde_json::json! avoids hand-quoting paths that may contain spaces or
    // unicode. SystemTime::now() formatted as seconds-since-epoch keeps the
    // record sortable without pulling a date format dep at this site.
    let record = serde_json::json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "pid": std::process::id(),
        "transport": transport_str,
        "project_root": project_root.display().to_string(),
        "version": env!("CARGO_PKG_VERSION"),
    });
    let line = format!("{record}\n");
    match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::debug!(error=%e, path=%log_path.display(), "session log: write failed");
            }
        }
        Err(e) => tracing::debug!(error=%e, path=%log_path.display(), "session log: open failed"),
    }
}

/// Resolve the directory the session log lives in. Honors
/// `$XDG_CACHE_HOME` if set, falling back to `$HOME/.cache/dioxus-mcp`.
/// Returns `None` when neither is available (e.g. inside a container with
/// no HOME).
fn session_log_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("dioxus-mcp"));
    }
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(PathBuf::from(home).join(".cache").join("dioxus-mcp"))
}
