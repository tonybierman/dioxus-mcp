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

pub(crate) mod project;
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
            let handler = DioxusMcp::new(state);
            let service = handler.serve(stdio()).await?;
            service.waiting().await?;
        }
        Transport::Http => {
            let bind = cli.bind.clone();
            let svc = TowerToHyperService::new(StreamableHttpService::new(
                {
                    let state = state.clone();
                    move || Ok(DioxusMcp::new(state.clone()))
                },
                LocalSessionManager::default().into(),
                Default::default(),
            ));
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            tracing::info!(%bind, "streamable HTTP listening");
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
        }
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
